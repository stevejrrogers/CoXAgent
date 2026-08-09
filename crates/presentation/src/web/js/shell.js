// App shell: project switching, files browser, codemap, settings, boot.
// Split from index.html — classic script, load order matters (one shared scope).
function switchProject(id){PID=id;INIT_ACT=false;GOAL_LOADED_PID=null;localStorage.setItem("coxpid",id);closeProjMenu();renderProjBtn();loadBudget();loadComments();connect();if(CUR==="codemap"&&window._cmTab==="files"){window._wsPath="";loadWorkspace("");}
  termKill();if(CUR==="terminal")openTerminal();
  // Chat is system-wide; switching project only refreshes which project channels show.
  loadChannels();}
function fileIcon(name){const e=(name.split(".").pop()||"").toLowerCase();
  if(["rs"].includes(e))return "brand-rust";if(["ts","tsx","js","jsx"].includes(e))return "brand-javascript";
  if(["json"].includes(e))return "braces";if(["md"].includes(e))return "markdown";
  if(["toml","yaml","yml","lock","cfg","conf","env"].includes(e))return "settings";
  if(["swift","kt","go","py","rb","java","cs"].includes(e))return "file-code";
  if(["png","jpg","jpeg","svg","gif","webp"].includes(e))return "photo";
  if(["dockerfile"].includes(e)||name.toLowerCase()==="dockerfile")return "brand-docker";
  return "file";}
async function loadWorkspace(path){
  const el=document.getElementById("ws-panel");if(!el)return;
  window._wsPath=path||"";
  let w=null;try{w=await(await fetch(api("/workspace")+(path?"?path="+encodeURIComponent(path):""))).json();}catch(e){}
  if(!w||!w.codebase){el.innerHTML="";return;}
  // Breadcrumb from the current relative path.
  const parts=(w.path||"").split("/").filter(Boolean);
  let acc="";const crumbs=[`<span class="wscrumb" onclick="loadWorkspace('')"><i class="ti ti-folder-code"></i> root</span>`]
    .concat(parts.map(seg=>{acc=acc?acc+"/"+seg:seg;const p=acc;return `<span class="wssep">/</span><span class="wscrumb" onclick="loadWorkspace('${esc(p)}')">${esc(seg)}</span>`;})).join("");
  const rows=(w.entries||[]).map(e=>{const child=(w.path?w.path+"/":"")+e.name;
    return e.dir
      ? `<div class="wsrow" onclick="loadWorkspace('${esc(child)}')"><i class="ti ti-folder" style="color:var(--amber)"></i><span class="wsname">${esc(e.name)}</span><i class="ti ti-chevron-right wsarr"></i></div>`
      : `<div class="wsrow" onclick="openFile('${esc(child)}')"><i class="ti ti-${fileIcon(e.name)}" style="color:var(--accent2)"></i><span class="wsname">${esc(e.name)}</span><span class="wssize">${fmtBytes(e.size||0)}</span></div>`;
  }).join("")||'<div class="empty">empty folder</div>';
  el.innerHTML=`<div class="sec">Workspace &amp; source ${w.is_git?'<span class="fchip" style="padding:2px 9px;font-size:10px">git</span>':''}</div>
    <div class="panel">
      <div class="wspath"><code id="ws-path">${esc(w.codebase)}</code><button onclick="copyText('ws-path',this)" title="Copy path"><i class="ti ti-copy"></i></button></div>
      <div class="wscrumbs">${crumbs}</div>
      <div class="wslist">${rows}</div>
      <div style="font-size:11px;color:var(--dim);margin-top:10px">Agents read &amp; write real files here — click a file to preview, a folder to open it.</div>
    </div>`;
}
async function openFile(path){
  const body=document.getElementById("file-body");const title=document.getElementById("file-title");
  title.textContent=path;body.textContent="loading…";document.getElementById("ov-file").classList.add("open");
  try{const r=await fetch(api("/file")+"?path="+encodeURIComponent(path));
    body.textContent=r.ok?(await r.json()).content:(r.status===415?"(binary file — can't preview)":r.status===413?"(file too large to preview)":"(unavailable)");}
  catch(e){body.textContent="(error)";}
}
function copyText(id,btn){const el=document.getElementById(id);if(!el)return;
  navigator.clipboard.writeText(el.textContent).then(()=>{const o=btn.innerHTML;btn.innerHTML='<i class="ti ti-check"></i>';setTimeout(()=>btn.innerHTML=o,1200);toast("Copied to clipboard","ok");}).catch(()=>{});}
function renderProjBtn(){
  const p=PROJECTS.find(x=>x.id===PID);
  document.getElementById("proj-mk").textContent=p?projInitial(p):"P";
  document.getElementById("proj-name").textContent=p?p.name:(PROJECTS.length?"Select project":"No projects");
  document.getElementById("proj-count").textContent=p?(p.alias?p.alias:"active project"):(PROJECTS.length+" available");
  // Header identity block on the left: big project name + coloured mark.
  const proj=document.getElementById("tb-proj");const mark=document.getElementById("tb-mark");
  if(proj)proj.textContent=p?p.name:(PROJECTS.length?"Select project":"No project");
  if(mark){mark.textContent=p?projInitial(p):"·";mark.style.background=p?`linear-gradient(135deg,${projColor(p.id)},color-mix(in srgb,${projColor(p.id)} 55%,#000))`:"var(--card2)";}
  document.title="CoXAgent"+(p?" · "+p.name:"");
  renderProjRail();
}
// Header project switcher — a single segmented control (not floating pills, not
// a dropdown): one click switches, active segment filled, a colour dot keeps
// each project recognisable, and an inline × (on hover) removes one.
// Up to this many pills stay inline; beyond that the rest collapse behind a
// "+N" button with a searchable menu (the active project is always a pill).
const MAX_PROJ_PILLS=5;
function renderProjRail(){
  const rail=document.getElementById("proj-rail");if(!rail)return;
  if(!PROJECTS.length){rail.innerHTML='<span class="segproj-empty">No projects yet</span>';closeProjMore();return;}
  let vis=PROJECTS,more=[];
  if(PROJECTS.length>MAX_PROJ_PILLS){
    vis=PROJECTS.slice(0,MAX_PROJ_PILLS);
    if(PID&&!vis.some(p=>p.id===PID)){
      const act=PROJECTS.find(p=>p.id===PID);
      if(act)vis=vis.slice(0,MAX_PROJ_PILLS-1).concat([act]);
    }
    const shown=new Set(vis.map(p=>p.id));
    more=PROJECTS.filter(p=>!shown.has(p.id));
  }
  const pill=p=>{const on=p.id===PID;const label=p.alias||p.name;
    return `<button class="segseg${on?' on':''}" title="${esc(p.name)}" onclick="switchProject('${esc(p.id)}')">
      <span class="sdot" style="background:${projColor(p.id)}"></span><span>${esc(label)}</span>
      <i class="ti ti-x sdel" title="Remove ${esc(p.name)}" onclick="event.stopPropagation();deleteProject('${esc(p.id)}')"></i>
    </button>`;};
  rail.innerHTML=vis.map(pill).join("")
    +(more.length?`<button class="segmore" id="proj-more-btn" title="${more.length} more projects" onclick="toggleProjMore(event)"><i class="ti ti-dots"></i> +${more.length}</button>`:"");
  if(!more.length)closeProjMore();
  const act=rail.querySelector(".segseg.on");if(act)act.scrollIntoView({inline:"nearest",block:"nearest"});
}
function toggleProjMore(e){
  e.stopPropagation();
  const m=document.getElementById("proj-more-menu");if(!m)return;
  if(!m.hidden){closeProjMore();return;}
  m.hidden=false;window._pmq="";renderProjMore();
  const b=document.getElementById("proj-more-btn");if(b)b.classList.add("on");
  setTimeout(()=>{const i=m.querySelector("input");if(i)i.focus();
    document.addEventListener("click",closeProjMoreOnOutside);},0);
}
function renderProjMore(){
  const m=document.getElementById("proj-more-menu");if(!m||m.hidden)return;
  const q=(window._pmq||"").toLowerCase();
  const list=PROJECTS.filter(p=>!q||(p.alias||"").toLowerCase().includes(q)||p.name.toLowerCase().includes(q));
  m.innerHTML=`<div class="pm-search"><i class="ti ti-search"></i><input placeholder="Tìm project…" value="${esc(window._pmq||"")}" oninput="window._pmq=this.value;renderProjMore()" onclick="event.stopPropagation()"></div>
    <div class="pm-list">${list.map(p=>`<button class="pm-item${p.id===PID?' on':''}" onclick="closeProjMore();switchProject('${esc(p.id)}')">
      <span class="sdot" style="background:${projColor(p.id)}"></span><span>${esc(p.alias||p.name)}</span></button>`).join("")||'<span class="segproj-empty">no match</span>'}</div>`;
  const i=m.querySelector("input");if(i){const v=i.value;i.focus();i.setSelectionRange(v.length,v.length);}
}
function closeProjMore(){
  const m=document.getElementById("proj-more-menu");if(m)m.hidden=true;
  const b=document.getElementById("proj-more-btn");if(b)b.classList.remove("on");
  document.removeEventListener("click",closeProjMoreOnOutside);
}
function closeProjMoreOnOutside(e){
  const m=document.getElementById("proj-more-menu");
  if(m&&!m.contains(e.target))closeProjMore();
}
// Stable per-project colour for its switcher dot.
function projColor(id){const pal=["#38bdf8","#a78bfa","#f472b6","#34d399","#fbbf24","#fb7185"];
  let h=0;for(let i=0;i<(id||"").length;i++)h=(h*31+id.charCodeAt(i))>>>0;return pal[h%pal.length];}
// ── Project goal (the brief agents are seeded with; from project_context.md) ──
let GOAL_LOADED_PID=null;
async function loadGoal(){
  const el=document.getElementById("ov-goal");if(!el)return;
  if(GOAL_LOADED_PID===PID)return;GOAL_LOADED_PID=PID;
  let d={goal:""};try{d=await(await fetch(api("/context"))).json();}catch(e){}
  renderGoal(d.goal||"");
}
function toasty(m,t){if(typeof toast==="function")toast(m,t);}
function renderGoal(goal){
  const el=document.getElementById("ov-goal");if(!el)return;el.dataset.goal=goal||"";
  if(!goal){el.innerHTML=`<div class="goalcard"><div class="gc-h"><i class="ti ti-target"></i> Project goal</div>
    <div class="gc-body gc-empty">No goal set yet — <a onclick="editGoal()">add one</a> to steer the agents.</div></div>`;return;}
  el.innerHTML=`<div class="goalcard"><div class="gc-h"><i class="ti ti-target"></i> Project goal
    <button class="gc-edit admin-only" onclick="editGoal()" title="Edit goal"><i class="ti ti-pencil"></i></button></div>
    <div class="gc-body">${esc(goal)}</div></div>`;
}
function editGoal(){
  const el=document.getElementById("ov-goal");const cur=el.dataset.goal||"";
  el.innerHTML=`<div class="goalcard"><div class="gc-h"><i class="ti ti-target"></i> Project goal</div>
    <textarea id="gc-input" class="gc-ta" rows="4" placeholder="What is this project building toward?">${esc(cur)}</textarea>
    <div class="gc-actions"><span class="gc-note"><i class="ti ti-info-circle"></i> Seeds new agent work after the next restart.</span>
      <button onclick="cancelGoal()" class="gc-btn">Cancel</button>
      <button onclick="saveGoal()" class="gc-btn pri">Save goal</button></div></div>`;
  document.getElementById("gc-input").focus();
}
function cancelGoal(){renderGoal(document.getElementById("ov-goal").dataset.goal||"");}
async function saveGoal(){
  const val=(document.getElementById("gc-input").value||"").trim();
  if(!val){toasty("Goal can't be empty","err");return;}
  try{const r=await fetch(api("/context"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({goal:val})});
    if(r.ok){renderGoal(val);toasty("Project goal updated","ok");}
    else{toasty("Save failed ("+r.status+")","err");}
  }catch(e){toasty("Network error","err");}
}
function toggleProjMenu(e){e.stopPropagation();const m=document.getElementById("proj-menu");if(m.classList.contains("open")){closeProjMenu();return;}renderProjMenu();m.classList.add("open");}
function closeProjMenu(){document.getElementById("proj-menu").classList.remove("open");}
document.addEventListener("click",e=>{const pk=document.querySelector(".projpick");if(pk&&!pk.contains(e.target))closeProjMenu();});
function renderProjMenu(){
  const m=document.getElementById("proj-menu");
  if(!PROJECTS.length){m.innerHTML='<div class="projitem" style="cursor:default;color:var(--muted)">No projects yet</div>';return;}
  m.innerHTML=PROJECTS.map(p=>`<div class="projitem${p.id===PID?' sel':''}" onclick="switchProject('${esc(p.id)}')">
    <span class="pi-mk">${esc(projInitial(p))}</span>
    <span class="pi-meta"><span class="pi-name">${esc(p.name)}</span><span class="pi-sub">${p.alias?esc(p.alias):esc(p.id)}</span></span>
    <i class="ti ti-check pi-check"></i>
    <i class="ti ti-trash pi-del" title="Remove project" onclick="event.stopPropagation();deleteProject('${esc(p.id)}')"></i></div>`).join("");
}
async function loadProjects(){
  let list=[];try{list=await(await fetch("/api/projects")).json();}catch(e){}
  PROJECTS=list;
  // System chat + meetings are WORKSPACE-level: connect the live socket even
  // with zero projects, so invites/reminders/rings always reach the user.
  if(!list.length){renderProjBtn();initChatBackground();return;}
  const saved=localStorage.getItem("coxpid");
  PID=(saved&&list.some(p=>p.id===saved))?saved:list[0].id;
  renderProjBtn();
  loadBudget();loadComments();connect();initChatBackground();
}
const TK_ROLE_COLOR={BA:"var(--accent2)",PO:"var(--amber)",SA:"var(--purple)",PD:"var(--green)"};
// Compact rich editor for the ticket Details field (self-contained; shares
// mdRender/htmlToMd with Docs but no live-doc side effects).
function tkRich(){return document.getElementById("nt-desc");}
function tkFmt(cmd){document.execCommand(cmd,false,null);tkRich().focus();}
function tkCode(){const s=window.getSelection();const t=s&&s.toString();document.execCommand("insertHTML",false,t?'<code>'+esc(t)+'</code>&nbsp;':'<code>code</code>&nbsp;');tkRich().focus();}
async function tkLink(){const s0=window.getSelection();const saved=(s0&&s0.rangeCount)?s0.getRangeAt(0).cloneRange():null;
  const url=await coxModal({title:"Insert link",message:"URL để chèn vào ticket.",input:{placeholder:"https://…",value:"https://"},confirmText:"Insert"});if(!url)return;
  if(saved){const s=window.getSelection();s.removeAllRanges();s.addRange(saved);}
  const s=window.getSelection();if(s&&s.toString())document.execCommand("createLink",false,url);else document.execCommand("insertHTML",false,'<a href="'+esc(url)+'" target="_blank" rel="noopener">'+esc(url)+'</a>&nbsp;');tkRich().focus();}
function ntDescGet(){return htmlToMd(tkRich()).trim();}
function ntDescSet(md){const r=tkRich();if(r)r.innerHTML=md?mdRender(md):"";}
async function teamAnalyze(){
  const idea=(ntDescGet()||document.getElementById("nt-title").value).trim();
  const err=document.getElementById("nt-err");const btn=document.getElementById("nt-analyze");const lbl=document.getElementById("nt-analyze-label");
  if(!idea){err.textContent="Write the idea first, then let the team analyze.";return;}
  err.textContent="";btn.disabled=true;
  const t0=Date.now();const tick=setInterval(()=>{lbl.innerHTML='BA · PO · SA · PD refining… '+Math.round((Date.now()-t0)/1000)+'s';},1000);
  lbl.textContent="BA · PO · SA · PD refining…";btn.querySelector("i").className="ti ti-loader-2 att-spin";
  try{
    const r=await fetch(api("/ticket-refine"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({description:idea})});
    if(!r.ok){err.textContent="Refine failed: "+(await r.text()||r.status)+" (needs a configured engine).";}
    else{
      const p=await r.json();
      if(p.title)document.getElementById("nt-title").value=p.title;
      if(p.description)ntDescSet(p.description);
      if(p.priority)document.getElementById("nt-prio").value=p.priority;
      if(p.complexity)document.getElementById("nt-cx").value=p.complexity;
      document.getElementById("nt-ui").checked=!!p.has_ui;
      if(p.acceptance_criteria&&p.acceptance_criteria.length)document.getElementById("nt-ac").value=p.acceptance_criteria.join("\n");
      const nb=document.getElementById("nt-notes");
      if(p.team_notes&&p.team_notes.length){
        nb.style.display="block";
        nb.innerHTML='<div class="tk-notes-h"><i class="ti ti-users"></i> The team weighed in</div>'+p.team_notes.map(n=>
          `<div class="tk-note"><span class="tk-role" style="color:${TK_ROLE_COLOR[n.role]||'var(--muted)'}">${esc(n.role)}</span><span>${esc(n.note)}</span></div>`).join("");
      }
      err.style.color="var(--green)";err.textContent="Refined by the team — review, then Save or Save & Start Flow.";
      setTimeout(()=>{err.style.color="var(--red)";err.textContent="";},5000);
    }
  }catch(e){err.textContent="Network error.";}
  clearInterval(tick);btn.disabled=false;btn.querySelector("i").className="ti ti-sparkles";lbl.textContent="Re-analyze with the team";
}
function openNewTicket(){["nt-title","nt-ac"].forEach(i=>document.getElementById(i).value="");ntDescSet("");document.getElementById("nt-err").textContent="";document.getElementById("nt-ui").checked=false;const nb=document.getElementById("nt-notes");nb.style.display="none";nb.innerHTML="";document.getElementById("nt-analyze-label").textContent="Let the team analyze & refine";document.getElementById("ov-newticket").classList.add("open");setTimeout(()=>document.getElementById("nt-title").focus(),50);}
async function saveTicket(startFlow){
  const title=document.getElementById("nt-title").value.trim();
  const err=document.getElementById("nt-err");
  if(!title){err.textContent="Title is required — write an idea or let the team analyze.";return;}
  const ac=(document.getElementById("nt-ac").value||"").split("\n").map(l=>l.trim()).filter(Boolean).slice(0,6);
  const body={title,description:ntDescGet(),
    ticket_type:document.getElementById("nt-type").value,
    priority:document.getElementById("nt-prio").value,
    complexity:document.getElementById("nt-cx").value,
    has_ui:document.getElementById("nt-ui").checked,
    acceptance_criteria:ac};
  err.style.color="var(--red)";err.textContent=startFlow?"Saving & starting…":"Saving…";
  try{
    const r=await fetch(api("/tickets"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(body)});
    if(!r.ok){err.textContent=await r.text()||"Failed.";return;}
    if(startFlow){
      try{await fetch(api("/control/resume"),{method:"POST"});}catch(e){}
    }
    close_("ov-newticket");toast(startFlow?"Ticket created — flow started":"Ticket created","ok");
    const s=await(await fetch(api("/state"))).json();render(s);
    if(startFlow){try{const rn=await(await fetch(api("/runner"))).json();renderRunner(rn);}catch(e){}}
  }catch(e){err.textContent="Network error.";}
}
let PROJ_MODE="new";
let IMPORT_SRC="folder";
function setImportSrc(sc){IMPORT_SRC=sc;
  document.getElementById("nps-folder").classList.toggle("on",sc==="folder");
  document.getElementById("nps-git").classList.toggle("on",sc==="git");
  document.getElementById("np-path-row").style.display=(PROJ_MODE==="import"&&sc==="folder")?"flex":"none";
  document.getElementById("np-git-row").style.display=(PROJ_MODE==="import"&&sc==="git")?"flex":"none";}
function setProjMode(m){PROJ_MODE=m;
  document.getElementById("npm-new").classList.toggle("on",m==="new");
  document.getElementById("npm-import").classList.toggle("on",m==="import");
  document.getElementById("np-src-row").style.display=m==="import"?"":"none";
  setImportSrc(IMPORT_SRC);
  // Goal (+ AI drafting) applies to both modes — an imported codebase needs a
  // brief just as much; it merges into the auto-detected comprehension context.
  document.getElementById("np-goal").placeholder=m==="import"
    ?"Hướng đi tiếp cho codebase này — e.g. 'ổn định hoá, thêm thanh toán, mobile app'. Click Draft để BA/PO viết brief."
    :"Rough idea — e.g. 'a secure team chat, more private than WhatsApp'. Click Draft to have BA/PO turn it into a project brief you review.";
  document.getElementById("np-submit-label").textContent=m==="import"?"Import project":"Create project";}
function openNewProject(){["np-name","np-alias","np-path","np-goal"].forEach(i=>document.getElementById(i).value="");document.getElementById("np-err").textContent="";setProjMode("new");document.getElementById("ov-newproj").classList.add("open");setTimeout(()=>document.getElementById("np-name").focus(),50);return loadNpSpaces();}
// Space picker in the New-project form: only the spaces the caller may file
// into (all for super, own for a space admin); hidden when none exist.
async function loadNpSpaces(){
  const row=document.getElementById("np-space-row"),sel=document.getElementById("np-space");
  row.style.display="none";sel.innerHTML="";
  let d=null;try{d=await(await fetch("/api/spaces")).json();}catch(e){return;}
  const sps=(d&&d.spaces)||[];if(!sps.length)return;
  // Every project must belong to a space — no unassigned option.
  sel.innerHTML=sps.map(s=>`<option value="${esc(s.id)}">${esc(s.name)}</option>`).join("");
  sel.value=sps[0].id;
  row.style.display="";
}
async function refineGoal(){
  const goal=document.getElementById("np-goal").value.trim();
  const err=document.getElementById("np-err");const lbl=document.getElementById("np-goal-label");
  if(!goal){err.textContent="Write a rough goal first.";return;}
  lbl.textContent="Drafting…";err.textContent="";
  try{
    const r=await fetch("/api/analyze-goal",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({goal})});
    if(!r.ok){err.textContent="Draft failed: "+(await r.text()||r.status)+" (needs a configured engine).";lbl.textContent="Draft with BA/PO";return;}
    const {brief}=await r.json();
    document.getElementById("np-goal").value=brief;
    err.style.color="var(--green)";err.textContent="BA/PO drafted a brief — review/edit, then create.";
    setTimeout(()=>{err.style.color="var(--red)";err.textContent="";},5000);
  }catch(e){err.textContent="Network error.";}
  lbl.textContent="Draft with BA/PO";
}
async function createProject(){
  const name=document.getElementById("np-name").value.trim();
  const alias=document.getElementById("np-alias").value.trim().toUpperCase();
  const err=document.getElementById("np-err");
  if(!name){err.textContent="Project name is required.";return;}
  const body={name,alias:alias||null};
  const sp=document.getElementById("np-space");if(sp&&sp.value)body.space=sp.value;
  if(PROJ_MODE==="import"){
    if(IMPORT_SRC==="git"){const u=document.getElementById("np-git").value.trim();
      if(!u){err.textContent="Enter the repository URL to import.";return;}body.git_url=u;}
    else{const p=document.getElementById("np-path").value.trim();
      if(!p){err.textContent="Enter the codebase path to import.";return;}body.existing=p;}}
  const g=document.getElementById("np-goal").value.trim();if(g)body.goal=g;
  err.style.color="var(--red)";err.textContent=PROJ_MODE==="import"?"Importing…":"Creating…";
  try{
    const r=await fetch("/api/projects",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(body)});
    if(!r.ok){const msg=await r.text()||"Failed.";err.textContent=msg;toast(msg,"err");return;}
    const {id}=await r.json();
    close_("ov-newproj");toast("Project created","ok");
    await loadProjects();
    if(id){switchProject(id);}
  }catch(e){err.textContent="Network error.";toast("Network error","err");}
}
async function deleteProject(id){
  id=id||PID;if(!id)return;
  const p=PROJECTS.find(x=>x.id===id);const nm=p?p.name:id;
  if(!await coxModal({title:"Remove project",message:'Gỡ project "'+nm+'" khỏi CoXAgent? File workspace vẫn còn trên disk; chỉ gỡ đăng ký.',danger:true,confirmText:"Remove"}))return;
  try{
    const r=await fetch("/api/projects/"+encodeURIComponent(id),{method:"DELETE"});
    if(!r.ok){toasty("Delete failed: "+(await r.text()||r.status),"err");return;}
    if(id===PID)localStorage.removeItem("coxpid");
    closeProjMenu();
    await loadProjects();
  }catch(e){toasty("Network error","err");}
}
let ME=null;
const NEXT_KEY="cox_next";
// Where the guest was trying to go when the auth gate intercepted them. The
// explicit ?next=<path> query param wins; otherwise we keep whatever hash or
// search the URL already carried. Saved so a successful login can return them
// there even when the redirect reloads (fresh location.hash) in between.
function rememberDestination(){
  try{
    const q=new URLSearchParams(location.search);
    const next=q.get("next"); // already percent-decoded by URLSearchParams
    // Accept either /path or #fragment form; otherwise keep whatever hash/search
    // the URL already carries so a reload between gate and login still returns.
    const target=(typeof next==="string"&&(next.startsWith("/")||next.startsWith("#")))
      ?next
      :((location.hash+location.search)||"/");
    if(target&&target!=="/")sessionStorage.setItem(NEXT_KEY,target);
  }catch(e){}
}
async function boot(){
  let r;try{r=await fetch("/api/auth/me");}catch(e){rememberDestination();showLogin();return;}
  if(r.status===401){rememberDestination();showLogin();return;}
  try{ME=await r.json();}catch(e){ME={auth:false};}
  try{updateSegments();}catch(e){}
  startApp();
}
function startApp(){
  document.getElementById("ov-login").classList.remove("open");
  applyRole();
  loadOpencodeModels(); // detect opencode providers after login
  const h=(location.hash||"").slice(1);if(TITLES[h])nav(h);else nav("overview");
  loadProjects();
  checkAgentSetup(false);
  ensureNotifPermission();
  checkAppUpdate();setInterval(checkAppUpdate,5*60*1000);
  window.addEventListener("focus",()=>checkAppUpdate());
  // Health is one cheap JSON with the hub's version in it: poll it, keep the
  // brand chip honest, and reload the window when the hub upgrades under it.
  const pollHealth=()=>fetch("/api/health").then(r=>r.json()).then(h=>{
    const b=document.getElementById("brand-ver");if(b&&h.version)b.textContent="v"+h.version;
    hubUpgradeReload(h.version);
  }).catch(()=>{});
  pollHealth();setInterval(pollHealth,30*1000);
  setTimeout(centerContent,200);
}
const AGENT_CLIS={claude:{label:"Claude Code",desc:"Anthropic's coding agent",install:"curl -fsSL https://claude.ai/install.sh | bash",docs:"https://claude.com/claude-code"},
  opencode:{label:"opencode",desc:"open-source multi-model agent",install:"curl -fsSL https://opencode.ai/install | bash",docs:"https://opencode.ai"}};
// Which machine each engine was found on: "hub" for the server's own PATH, else
// the runner that reported it. Lets the wizard say "installed · luton@mac"
// instead of implying it sits on the machine you are reading this from.
function engineHosts(list){
  const m={};
  for(const e of (list||[])){ if(e&&e.name) m[String(e.name).toLowerCase()]=e.where||"hub"; }
  return m;
}
async function checkAgentSetup(force){
  let list=[];try{list=await(await fetch("/api/engines")).json();}catch(e){}
  const have=new Set((list||[]).map(e=>e.name.toLowerCase()));
  window._engines=have; window._engineHosts=engineHosts(list);
  const dismissed=localStorage.getItem("coxSetupDone")==="1";
  const runnable=[...have].some(n=>n==="claude"||n==="opencode");
  if(runnable)localStorage.setItem("coxSetupDone","1");
  // An empty list used to be auto-dismissed, because a hub in a container
  // detected nothing and nagged every login. /api/engines now also reports what
  // each live runner found on ITS machine, so empty finally means what it says:
  // no agent CLI anywhere on this team. That is precisely when the guide helps.
  if(!force&&(runnable||dismissed))return;
  renderSetupWizard(have);
  document.getElementById("ov-setup").classList.add("open");
}
// Which OS is reading this page. The shell one-liners below are macOS/Linux
// only; on Windows they are worse than useless — they look like something you
// could paste. There the download page is the honest primary path.
function setupPlatform(){
  const s=(navigator.userAgentData&&navigator.userAgentData.platform)||navigator.platform||"";
  if(/win/i.test(s))return "windows";
  if(/mac/i.test(s))return "macos";
  return "linux";
}
function renderSetupWizard(have){
  const win=setupPlatform()==="windows";
  const rows=Object.entries(AGENT_CLIS).map(([k,c])=>{
    const ok=have.has(k);
    // Where an engine was found matters once runners are remote: "installed"
    // on someone else's machine is not something to install again here.
    const on=(window._engineHosts&&window._engineHosts[k])||"";
    const pill=ok
      ?`<span class="setpill on">installed${on&&on!=="hub"?" · "+esc(workerLabel(on,Object.values(window._engineHosts||{}))):""}</span>`
      :'<span class="setpill">not found</span>';
    const dl=`<a href="${c.docs}" target="_blank" rel="noopener" class="setlink"><i class="ti ti-download" style="font-size:12px"></i> Download for ${win?"Windows":setupPlatform()==="macos"?"macOS":"Linux"}</a>`;
    return `<div class="setrow ${ok?'ok':''}">
      <div class="setmk">${ok?'<i class="ti ti-check"></i>':'<i class="ti ti-download"></i>'}</div>
      <div style="flex:1;min-width:0"><div class="setname">${esc(c.label)} ${pill}</div>
        <div class="setdesc">${esc(c.desc)}</div>
        ${ok?'':(win
          ?`${dl}<div class="setdesc" style="margin-top:6px">The one-line installer is macOS/Linux only.</div>`
          :`<div class="setcmd"><code id="cmd-${k}">${esc(c.install)}</code><button onclick="copyCmd('${k}')" title="Copy"><i class="ti ti-copy"></i></button></div>
            <div style="display:flex;gap:12px;flex-wrap:wrap">${dl}
            <a href="${c.docs}" target="_blank" rel="noopener" class="setlink">Installation guide <i class="ti ti-external-link" style="font-size:12px"></i></a></div>`)}</div></div>`;
  }).join("");
  const anyOk=[...have].some(n=>n==="claude"||n==="opencode");
  document.getElementById("setup-body").innerHTML=`
    <div class="sethero"><div class="seticon"><i class="ti ti-robot"></i></div>
      <h3>Connect a coding agent</h3>
      <p>CoXAgent runs local agent CLIs to do the work. ${anyOk?'You\'re ready to go — an agent was detected.':'Install one below (Claude Code is free to start), then re-check.'}</p></div>
    ${rows}
    <div class="setnote"><i class="ti ti-info-circle"></i> Run the command in your terminal, then press <b>Re-check</b>. CoXAgent detects CLIs on your <code>PATH</code>.</div>
    <div style="margin-top:14px;text-align:center">
      <button class="btn-ghost" onclick="dismissSetup()" style="font-size:13px">I'll set it up later</button>
    </div>`;
}
function copyCmd(k){const el=document.getElementById("cmd-"+k);if(!el)return;
  navigator.clipboard.writeText(el.textContent).then(()=>{el.parentElement.classList.add("copied");setTimeout(()=>el.parentElement.classList.remove("copied"),1200);}).catch(()=>{});}
async function recheckAgents(){
  const btn=document.getElementById("set-recheck");const old=btn.innerHTML;btn.innerHTML='<i class="ti ti-loader-2"></i> Checking…';
  let list=[];try{list=await(await fetch("/api/engines")).json();}catch(e){}
  const have=new Set((list||[]).map(e=>e.name.toLowerCase()));window._engines=have;
  window._engineHosts=engineHosts(list);
  renderSetupWizard(have);btn.innerHTML=old;
  if([...have].some(n=>n==="claude"||n==="opencode")){localStorage.setItem("coxSetupDone","1");}
}
function dismissSetup(){localStorage.setItem("coxSetupDone","1");close_("ov-setup");}
function showLogin(){document.getElementById("ov-login").classList.add("open");setTimeout(()=>document.getElementById("lg-user").focus(),50);}
async function doLogin(){
  const username=document.getElementById("lg-user").value.trim();
  const password=document.getElementById("lg-pass").value;
  const totp=document.getElementById("lg-totp").value.trim();
  const err=document.getElementById("lg-err");
  if(!username||!password){err.textContent="Enter username and password.";return;}
  err.textContent="Signing in…";
  try{
    const body={username,password};if(totp)body.totp=totp;
    const r=await fetch("/api/auth/login",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(body)});
    if(!r.ok){
      let d={};try{d=await r.json();}catch(_){}
      if(d.totp_required){
        document.getElementById("lg-totp-row").style.display="flex";
        err.textContent="Enter your authenticator code.";
        setTimeout(()=>document.getElementById("lg-totp").focus(),50);
        return;
      }
      err.textContent="Invalid credentials.";return;
    }
    document.getElementById("lg-pass").value="";document.getElementById("lg-totp").value="";
    document.getElementById("lg-totp-row").style.display="none";
    // Return a guest to where they were headed before the gate intercepted
    // them (see rememberDestination): navigate there once auth succeeds.
    const next=sessionStorage.getItem(NEXT_KEY);
    sessionStorage.removeItem(NEXT_KEY);
    await boot();
    // A guest who asked for ?next=<hash> lands back on that view once signed
    // in; an invalid/nonexistent fragment safely falls through to overview.
    if(next){
      const frag=(next.indexOf("#")>=0)?next.slice(next.indexOf("#")+1).split("?")[0]:"";
      if(frag&&TITLES[frag])nav(frag);
    }
  }catch(e){err.textContent="Network error.";}
}
async function doLogout(){try{await fetch("/api/auth/logout",{method:"POST"});}catch(e){}location.reload();}
// Role capabilities (mirror of AuthRole in the backend).
const LEAD_ROLES=["director","manager","techlead","dslead","dalead"];
const ROLE_LABELS={super:"Super Admin",admin:"Admin",director:"Director",manager:"Manager",techlead:"Tech.Lead",dslead:"DS.Lead",dalead:"DA.Lead",ba:"BA",po:"PO",sa:"SA",sm:"SM",qa:"QA",fe:"FE",be:"BE",aie:"AIE",ds:"DS",da:"DA",de:"DE",reviewer:"Reviewer",viewer:"Viewer"};
function roleLabel(r){return ROLE_LABELS[r]||r;}
function roleCanWrite(r){return r!=="viewer";}
function roleCanCreateChannel(r){return r==="super"||r==="admin"||LEAD_ROLES.includes(r);}
function canCreateChannel(){return !ME||!ME.auth||roleCanCreateChannel(ME.role);}
// Management surfaces (Settings, Users) are Admin + lead tier only.
function roleCanManage(r){return r==="super"||r==="admin"||LEAD_ROLES.includes(r);}
function canManage(){return !ME||!ME.auth||roleCanManage(ME.role);}
// PR review actions (merge / request-changes / close / preview / force-merge)
// are Super, Admin, the lead tier and the legacy Reviewer — the exact mirror of
// AuthRole::can_review, which auth_mw enforces (COX-B038). Keep the two in step:
// a role shown a Merge button the server refuses is a 403 the user can't act on.
function roleCanReview(r){return r==="super"||r==="admin"||r==="reviewer"||LEAD_ROLES.includes(r);}
// Only the legacy read-only Viewer gets a locked-down UI; every real role writes.
function applyRole(){
  const isViewer=ME&&ME.auth&&ME.role==="viewer";
  // Admin surfaces (Audit, user mgmt) show for admins and in open/local mode.
  const isAdmin=!ME||!ME.auth||ME.role==="admin";
  document.body.classList.toggle("viewer",!!isViewer);
  document.body.classList.toggle("admin",!!isAdmin);
  document.body.classList.toggle("manage",canManage());
  // Show/hide create-channel button based on permissions.
  const btn=document.getElementById("btn-newchan");
  if(btn)btn.style.display=canCreateChannel()?"inline-flex":"none";
  const badge=document.getElementById("user-badge");
  if(ME&&ME.auth){badge.style.display="flex";
    document.getElementById("ub-name").textContent=ME.username;
    // Full label in the tooltip; the chip itself stays one short line so the
    // badge never wraps into a two-line block.
    const rc=document.getElementById("ub-role");rc.textContent=roleLabel(ME.role);rc.title=roleLabel(ME.role);
    rc.className="urole "+(ME.role==="super"||ME.role==="admin"?"admin":"viewer");
    renderMe();}
  else{badge.style.display="none";}
}
// Own badge: avatar (or initials) + status emoji; click opens the profile modal.
function renderMe(){const box=document.querySelector("#user-badge .uavatar");if(!box||!ME)return;
  const me=ME.username||"?";const p=PROFILES[me]||{};
  // Bind through a wrapper: assigning openProfile directly hands it the click
  // Event as `tab`, which matches no pane and opens the modal with nothing selected.
  box.style.cursor="pointer";box.title="Edit your profile & status";box.onclick=()=>openProfile("profile");
  box.innerHTML=p.avatar?`<img src="${esc(p.avatar)}" data-u="${esc(me)}" style="width:100%;height:100%;object-fit:cover;border-radius:inherit"
      onerror="this.remove()" onload="if(this.naturalWidth<8||this.naturalHeight<8)this.remove()">`
    :`<span id="ub-initials">${esc(me.slice(0,2).toUpperCase())}</span>`;
  const nm=document.getElementById("ub-name");
  if(nm)nm.innerHTML=esc(ME.name||me)+(p.status_emoji?` <span class="ustatus" title="${esc(p.status_text||"")}">${esc(p.status_emoji)}</span>`:"");
}
// Build <option>s for a role <select> (excludes legacy reviewer/viewer).
function roleOptions(sel){
  return Object.keys(ROLE_LABELS).filter(r=>r!=="reviewer"&&r!=="viewer")
    .map(r=>`<option value="${r}"${r===sel?' selected':''}>${roleLabel(r)}${r==="super"?" — hub-wide":(r==="admin"?" — full access":(roleCanCreateChannel(r)?" — lead":""))}</option>`).join("");
}
let USERS=[];
function renderUsers(){
  const list=document.getElementById("usr-list");if(!list)return;
  renderUserProjPicker();
  fetch("/api/auth/users").then(r=>r.ok?r.json():[]).then(rows=>{
    USERS=Array.isArray(rows)?rows:[];
    if(!USERS.length){list.innerHTML='<div class="empty">no accounts</div>';return;}
    list.innerHTML=USERS.map(u=>{const admin=u.role==="admin";
      const grad=admin?"linear-gradient(135deg,var(--amber),#b45309)":"linear-gradient(135deg,var(--accent),var(--accent2))";
      const me=ME&&ME.username===u.username;
      const un=encodeURIComponent(u.username);
      const mine=u.projects||[];
      const hasName=!!(u.name&&u.name.trim());
      const av=(hasName?u.name.trim():(u.username||'?')).slice(0,2).toUpperCase();
      // Admins implicitly see every project; viewers are scoped to assignments.
      const chips=admin?'<span class="uproj all"><i class="ti ti-infinity" style="font-size:12px"></i> all projects</span>'
        :(mine.length?mine.map(pid=>{const p=PROJECTS.find(x=>x.id===pid);const nm=p?p.name:pid;
            return `<span class="uproj"><span class="sdot" style="background:${projColor(pid)}"></span>${esc(nm)}<i class="ti ti-x" title="Unassign" onclick="unassignUser('${esc(pid)}','${un}')"></i></span>`;}).join("")
          :'<span class="uproj none">no projects yet</span>');
      const avail=PROJECTS.filter(p=>!mine.includes(p.id));
      const addSel=(!admin&&avail.length)?`<select class="uprojadd" onchange="if(this.value){assignUser(this.value,'${un}');this.value=''}"><option value="">＋ assign…</option>${avail.map(p=>`<option value="${esc(p.id)}">${esc(p.name)}</option>`).join("")}</select>`:'';
      const display=hasName?esc(u.name.trim()):esc(u.username);
      const sub=[hasName?'@'+esc(u.username):'',u.email?esc(u.email):''].filter(Boolean).join(' · ');
      return `<div class="memrow"><div class="memav" style="background:${grad}">${esc(av)}</div>
        <div class="meminfo"><div class="memname">${display}${me?'<span class="memyou">you</span>':''}<span class="memrole ${admin?'admin':'viewer'}">${esc(roleLabel(u.role))}</span>${u.twofa?'<span class="mem2fa">2FA</span>':''}</div>
          ${sub?`<div class="memsub">${sub}</div>`:''}
          <div class="memchips">${chips}${addSel}</div></div>
        <div class="memacts"><button class="memicon" onclick="editUser('${un}')" title="Edit user"><i class="ti ti-pencil"></i></button>
        ${me?'':`<button class="memicon danger" onclick="deleteUser('${un}')" title="Remove account"><i class="ti ti-trash"></i></button>`}</div></div>`;}).join("");
  }).catch(()=>{list.innerHTML='<div class="empty">unable to load users</div>';});
}
// Multi-select project checkboxes for the create form (a user can join many).
// Filters by the selected space in #usr-space.
async function renderUserProjPicker(){
  const el=document.getElementById("usr-projpick");if(!el)return;
  const spaceSel=document.getElementById("usr-space");
  const sid=spaceSel?spaceSel.value:"";
  let projects=PROJECTS;
  // If a space is selected, fetch that space's project list to filter.
  if(sid){
    try{
      const r=await fetch("/api/spaces");const d=await r.json();
      const sp=(d.spaces||[]).find(s=>s.id===sid);
      if(sp&&sp.projects&&sp.projects.length){
        const pidSet=new Set(sp.projects);
        projects=PROJECTS.filter(p=>pidSet.has(p.id));
      }
    }catch(e){}
  }
  if(!projects.length){el.innerHTML='<span style="font-size:12px;color:var(--dim)">no projects in this space</span>';return;}
  el.innerHTML=projects.map(p=>`<label class="uprojchk"><input type="checkbox" value="${esc(p.id)}"><span class="sdot" style="background:${projColor(p.id)}"></span>${esc(p.name)}</label>`).join("");
}
async function openInvite(){
  // Populate space dropdown from /api/spaces
  const spaceSel=document.getElementById("usr-space");
  if(spaceSel){
    spaceSel.innerHTML='<option value="">— all projects —</option>';
    try{
      const r=await fetch("/api/spaces");
      const d=await r.json();
      const spaces=(d&&d.spaces)||[];
      spaceSel.innerHTML='<option value="">— all projects —</option>'+
        spaces.map(s=>{var nm=esc(s.name);var n=(s.projects||[]).length;return `<option value="${esc(s.id)}">${nm} · ${n} project${n!==1?"s":""}</option>`;}).join("");
    }catch(e){}
  }
  renderUserProjPicker();
  document.getElementById("usr-role").innerHTML=roleOptions("ba");
  ["usr-fullname","usr-email","usr-name","usr-pass"].forEach(id=>{const e=document.getElementById(id);if(e)e.value="";});
  document.getElementById("usr-err").textContent="";
  document.getElementById("ov-invite").classList.add("open");
  setTimeout(()=>{const f=document.getElementById("usr-fullname");if(f)f.focus();},40);
}
// Rename the current project (admin-only pencil in the header).
function openRename(){const p=PROJECTS.find(x=>x.id===PID);if(!p)return;
  const inp=document.getElementById("rn-name");inp.value=p.name||"";
  document.getElementById("rn-msg").textContent="";
  document.getElementById("ov-rename").classList.add("open");
  setTimeout(()=>{inp.focus();inp.select();},40);
}
async function saveRename(){const name=(document.getElementById("rn-name").value||"").trim();
  const msg=document.getElementById("rn-msg");
  if(!name){msg.style.color="var(--red)";msg.textContent="Name can't be empty.";return;}
  try{const r=await fetch("/api/projects/"+encodeURIComponent(PID),{method:"PATCH",headers:{"Content-Type":"application/json"},body:JSON.stringify({name})});
    if(!r.ok){msg.style.color="var(--red)";msg.textContent=await r.text()||"Failed.";return;}
    close_("ov-rename");toasty("Project renamed","ok");await loadProjects();renderProjBtn();
  }catch(e){msg.style.color="var(--red)";msg.textContent="Network error.";}
}
// ── Code review: open PRs from the forge, approve / merge / request changes ──
// Open/local mode (no auth wired) passes every gate server-side, so the review
// buttons must show there too — same `!ME||!ME.auth` shape as canManage().
function canReview(){return !ME||!ME.auth||roleCanReview(ME.role);}
// ── Clean-base drain banner: BIG visible hold notice while a refactor waits
// on the merge queue. Every agent of every user is paused for new work — the
// banner says so and points at the way out (merge green PRs).
let DRAIN_CACHE={t:0,n:0};
function goalIsRefactor(){
  const g=(((STATE||{}).sprint||{}).goal||"")+" "+((STATE||{}).sprint_goal||"");
  return ((STATE||{}).refactor_mode===true)||/refactor|restructure|migrat|tái cấu trúc|cấu trúc lại/i.test(g);
}
async function drainBanner(elId){
  const el=document.getElementById(elId);if(!el)return;
  const sweepBtn=`<button class="gc-btn pri" onclick="mergeSweep()" title="SA merges every green PR right now (oldest first) — no tokens"><i class="ti ti-git-merge"></i> SA merge sweep</button>`;
  // The Review tab always offers the sweep, drain or not.
  const toolbar=elId==="rv-drain"?`<div style="display:flex;justify-content:flex-end;margin-bottom:10px">${sweepBtn}</div>`:"";
  if(!goalIsRefactor()){el.innerHTML=toolbar;return;}
  if(Date.now()-DRAIN_CACHE.t>15000){
    try{const prs=await(await fetch(api("/prs"))).json();DRAIN_CACHE={t:Date.now(),n:Array.isArray(prs)?prs.length:0};}catch(e){el.innerHTML=toolbar;return;}
  }
  el.innerHTML=(DRAIN_CACHE.n>0?`<div class="drainbar"><i class="ti ti-barrier-block"></i>
    <div><b>CLEAN-BASE DRAIN</b> — refactor đang chờ merge sạch <b>${DRAIN_CACHE.n} PR</b>.
    Mọi agent của mọi user tạm NGỪNG mở việc mới — chỉ fix conflict &amp; merge (cũ nhất trước).
    <a onclick="nav('review')">Mở Review để merge PR xanh →</a> ${sweepBtn}</div></div>`:"")+toolbar;
}
// Ask the SA to merge every green open PR right now (oldest first). Token-free;
// results are announced in #agents and summarised in a toast.
async function mergeSweep(){
  toasty("SA đang quét queue & merge PR xanh…","ok");
  try{
    const r=await fetch(api("/merge-sweep"),{method:"POST"});
    if(!r.ok){toasty("Sweep failed: "+(await r.text()||r.status),"err");return;}
    const d=await r.json();
    const m=(d.merged||[]).length,s=(d.skipped||[]).length;
    toasty(m?`Đã merge ${m} PR ✓${s?` · còn ${s} chưa đủ điều kiện`:""}`:`Chưa merge được PR nào${s?` — ${s} cái đang conflict/CI`:""}`,m?"ok":"warn");
    DRAIN_CACHE.t=0;
    if(CUR==="review")renderReview(); if(CUR==="overview")drainBanner("ov-drain");
  }catch(e){toasty("Network error","err");}
}
async function renderReview(){
  drainBanner("rv-drain");
  const el=document.getElementById("review-body");if(!el)return;
  el.innerHTML='<div class="empty">loading pull requests…</div>';
  let d={};try{d=await(await fetch(api("/prs"))).json();}catch(e){el.innerHTML='<div class="empty">unable to load</div>';return;}
  if(!d.configured){el.innerHTML=`<div class="rev-empty"><i class="ti ti-git-pull-request"></i><div>Git review isn't set up</div><span>Configure a repository in <a onclick="nav('settings')">Settings → Git &amp; version control</a> to open and review pull requests here.</span></div>`;return;}
  const prs=d.prs||[];
  const head=`<div class="sec">Pull requests <span style="font-size:11px;color:var(--dim);font-weight:400">· ${prs.length} open${d.error?' · <span style="color:var(--red)">'+esc(d.error)+'</span>':''}</span></div>`;
  if(!prs.length){el.innerHTML=head+`<div class="rev-empty"><i class="ti ti-check"></i><div>No open pull requests</div><span>Agent-shipped tickets will appear here for review.</span></div>`;return;}
  const ciBadge=c=>{const m={passing:["passing","var(--green)","circle-check"],failing:["failing","var(--red)","circle-x"],pending:["CI running","var(--amber)","loader"],none:["no CI","var(--dim)","minus"]}[c]||["",""];
    return `<span class="rev-ci" style="color:${m[1]}"><i class="ti ti-${m[2]}"></i> ${m[0]}</span>`;};
  const rev=canReview();
  // The SA's review verdict (a suggestion when auto-merge is off).
  const reviewBanner=r=>{if(!r)return"";const ok=r.decision==="approve";
    return `<div class="rev-verdict ${ok?'ok':'chg'}"><i class="ti ti-${ok?'circle-check':'arrow-back-up'}"></i>
      <div><b>SA ${ok?'approved':'requested changes'}</b>${d.auto_merge?'':(ok?' · awaiting your merge':' · agent will fix')}<div class="rev-vsum">${esc(r.summary||'')}</div></div></div>`;};
  el.innerHTML=head+`<div class="revlist">`+prs.map(p=>`
    <div class="revcard">
      <div class="revmain">
        <div class="revtitle"><a href="${esc(p.url)}" target="_blank" rel="noopener">${esc(p.title)}</a> <span class="revnum">#${p.number}</span></div>
        <div class="revmeta"><span class="revbranch"><i class="ti ti-git-branch"></i> ${esc(p.head)} → ${esc(p.base)}</span>
          ${ciBadge(p.ci)}
          ${p.mergeable?'':'<span class="rev-conflict"><i class="ti ti-alert-triangle"></i> conflicts</span>'}
          <span class="revby">by ${esc(p.author||'—')}</span></div>
        ${reviewBanner(p.review)}
      </div>
      <div class="revacts">
        <button class="gc-btn" onclick="viewDiff(${p.number},'${esc(p.head)}')"><i class="ti ti-file-diff"></i> Diff</button>
        ${rev?`<button class="gc-btn" title="Run THIS branch on the app port so you can see it before approving" onclick="prAction(${p.number},'preview')"><i class="ti ti-eye"></i> Preview</button>
        <button class="gc-btn" title="Stop the preview and restore the main build" onclick="prAction(${p.number},'preview-stop')"><i class="ti ti-eye-off"></i></button>
        <button class="gc-btn" onclick="prAction(${p.number},'request-changes')"><i class="ti ti-arrow-back-up"></i> Changes</button>
        <button class="gc-btn pri" ${p.mergeable?'':'disabled'} onclick="prAction(${p.number},'merge')"><i class="ti ti-git-merge"></i> Merge</button>
        <button class="gc-btn forcemg" title="SA gỡ conflict NGAY (nếu có) rồi merge PR này — theo dõi tiến trình trong #agents" onclick="prAction(${p.number},'force-merge')"><i class="ti ti-bolt"></i> Force</button>`:''}
      </div>
    </div>`).join("")+`</div>`;
}
async function viewDiff(num,head){
  document.getElementById("diff-title").innerHTML='<i class="ti ti-git-pull-request" style="color:var(--accent2)"></i> '+esc(head)+' · #'+num;
  const body=document.getElementById("diff-body");body.textContent="loading…";body.innerHTML="loading…";
  document.getElementById("ov-diff").classList.add("open");
  try{const d=await(await fetch(api("/prs/"+num+"/diff"))).json();body.innerHTML=renderDiff(d.diff||"(empty)");}
  catch(e){body.textContent="unable to load diff";}
}
// Minimal +/- colouring for a unified diff.
// Structured diff: grouped per file with +/- gutters and old/new line numbers.
function renderDiff(t){
  if(!t||t==="(empty)")return '<div class="empty" style="padding:20px">no changes</div>';
  const lines=t.split("\n");let out="",inFile=false,body="",fname="",adds=0,dels=0,oldN=0,newN=0;
  const flush=()=>{if(inFile)out+=`<div class="df-file"><div class="df-head"><i class="ti ti-file-code"></i> <span class="df-name">${esc(fname)}</span><span class="df-stat"><span class="df-a">+${adds}</span><span class="df-d">−${dels}</span></span></div><div class="df-body">${body}</div></div>`;};
  for(const l of lines){
    if(l.startsWith("diff --git")){flush();inFile=true;body="";adds=0;dels=0;fname=((l.match(/ b\/(.+)$/)||[])[1])||l;continue;}
    if(/^(index |new file|deleted file|similarity|rename |\+\+\+|---|Binary )/.test(l))continue;
    if(l.startsWith("@@")){const m=l.match(/@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)/);if(m){oldN=+m[1];newN=+m[2];}
      body+=`<div class="df-hunk"><span class="df-ln"></span><span class="df-ln"></span><span class="df-tx">${esc(m?('@@ '+(m[3]||'').trim()):l)}</span></div>`;continue;}
    let cls="ctx",g1="",g2="",tx=l.slice(1);
    if(l.startsWith("+")){cls="add";adds++;g2=newN++;}
    else if(l.startsWith("-")){cls="del";dels++;g1=oldN++;}
    else{g1=oldN++;g2=newN++;if(!l.length)tx="";}
    body+=`<div class="df-row ${cls}"><span class="df-ln">${g1}</span><span class="df-ln">${g2}</span><span class="df-tx">${esc(tx)}</span></div>`;
  }
  flush();return out||'<div class="empty" style="padding:20px">no changes</div>';}
async function prAction(num,action){
  let comment="";
  if(action==="request-changes"){
    comment=await coxModal({title:"Request changes on PR #"+num,message:"Mô tả rõ cần sửa gì — DEV agent sẽ đọc và tự xử lý ở cycle tới.",input:{placeholder:"e.g. thêm test cho case token hết hạn; đừng đổi API public",multiline:true},confirmText:"Request changes"});
    if(comment===null||comment==="")return;}
  if(action==="merge"&&!(await coxModal({title:"Merge PR #"+num+"?",message:"Squash-merge vào main và xoá branch — chính thức ship code này.",confirmText:"Merge"})))return;
  if(action==="force-merge"&&!(await coxModal({title:"⚡ Force-merge PR #"+num+"?",message:"SA sẽ gỡ conflict ngay (nếu có) rồi merge thẳng vào main — kể cả khi chưa có CI check. Theo dõi tiến trình trong #agents.",danger:true,confirmText:"Force merge"})))return;
  if(action==="preview")toasty("Building preview of PR #"+num+"… (docker build, may take a minute)","ok");
  try{const r=await fetch(api("/prs/"+num+"/"+action),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({comment})});
    if(r.ok){
      if(action==="preview"){const d=await r.json().catch(()=>({}));
        toasty("Preview live"+(d.url?" — "+d.url:""),"ok");if(d.url)window.open(d.url,"_blank");}
      else if(action==="force-merge"){toasty("⚡ SA đang force-merge #"+num+" — xem tiến trình trong #agents","ok");}
      else toasty(action==="merge"?"Merged ✓":action==="request-changes"?"Changes requested — a DEV agent will address it next cycle":action==="preview-stop"?"Preview stopped — main restored":"Done","ok");
      renderReview();}
    else{toasty("Failed: "+(await r.text()||r.status),"err");}
  }catch(e){toasty("Network error","err");}
}
// ── Embedded terminal (IDE-style): xterm.js ↔ WS ↔ server PTY ────────────────
// Admin/Super only (server-enforced). One live session per app; switching
// projects or clicking New session replaces it. xterm.js loads lazily from CDN
// the first time the view opens, so app boot stays light.
let TERMS=[],TERM_SEQ=0,TERM_ACTIVE=null,TERM_LOADING=false;
function termLib(){
  if(window.Terminal&&window.FitAddon)return Promise.resolve();
  if(TERM_LOADING)return TERM_LOADING;
  // Served from the hub binary itself — works offline/air-gapped, no CDN.
  const css=document.createElement("link");css.rel="stylesheet";
  css.href="/assets/xterm.min.css";
  document.head.appendChild(css);
  const load=src=>new Promise((res,rej)=>{const s=document.createElement("script");s.src=src;s.onload=res;s.onerror=rej;document.head.appendChild(s);});
  TERM_LOADING=load("/assets/xterm.min.js")
    .then(()=>load("/assets/xterm-addon-fit.min.js"))
    .catch(e=>{TERM_LOADING=false;throw e;});
  return TERM_LOADING;
}
function termTabsRender(){
  const box=document.getElementById("term-tabs");if(!box)return;
  box.innerHTML=TERMS.map(t=>`<span class="term-tab${t.id===TERM_ACTIVE?" on":""}${t.live?" live":""}" onclick="termSelect(${t.id})">
    <span class="tdot"></span>${esc(t.name)}
    <i class="ti ti-x tx" onclick="event.stopPropagation();termClose(${t.id})"></i></span>`).join("");
  const lbl=document.getElementById("term-label");
  const a=TERMS.find(t=>t.id===TERM_ACTIVE);
  if(lbl)lbl.textContent=a?`${a.pid} · ${a.term.cols}×${a.term.rows}`:"";
}
async function termNew(){
  if(!PID){toasty("Chọn project trước","warn");return;}
  try{await termLib();}catch(e){toasty("Không tải được xterm.js (cần mạng)","err");return;}
  const id=++TERM_SEQ;
  const host=document.getElementById("term-host");
  const pane=document.createElement("div");pane.className="term-pane";pane.id="term-pane-"+id;
  host.appendChild(pane);
  const term=new window.Terminal({fontFamily:"ui-monospace,Menlo,monospace",fontSize:12.5,cursorBlink:true,
    theme:{background:"#0b0e14",foreground:"#d8dee9",cursor:"#38bdf8",selectionBackground:"#264f78"}});
  const fit=new window.FitAddon.FitAddon();term.loadAddon(fit);
  term.open(pane);
  const proto=location.protocol==="https:"?"wss":"ws";
  const ws=new WebSocket(`${proto}://${location.host}/api/projects/${encodeURIComponent(PID)}/terminal`);
  ws.binaryType="arraybuffer";
  const t={id,name:"zsh "+id,pid:PID,term,fit,ws,live:false};
  TERMS.push(t);
  ws.onopen=()=>{t.live=true;fit.fit();ws.send(JSON.stringify({resize:{cols:term.cols,rows:term.rows}}));termTabsRender();term.focus();};
  ws.onmessage=e=>{term.write(typeof e.data==="string"?e.data:new Uint8Array(e.data));};
  ws.onclose=e=>{t.live=false;
    term.write("\r\n\x1b[2m["+(e.code===1008?"read-only role":"session closed")+" — bấm + để mở terminal mới]\x1b[0m\r\n");
    termTabsRender();};
  ws.onerror=()=>{t.live=false;termTabsRender();};
  term.onData(d=>{if(ws.readyState===1)ws.send(JSON.stringify({input:d}));});
  term.onResize(()=>{if(ws.readyState===1)ws.send(JSON.stringify({resize:{cols:term.cols,rows:term.rows}}));termTabsRender();});
  termSelect(id);
}
function termSelect(id){
  TERM_ACTIVE=id;
  TERMS.forEach(t=>{const p=document.getElementById("term-pane-"+t.id);if(p)p.hidden=t.id!==id;});
  const a=TERMS.find(t=>t.id===id);
  if(a){try{a.fit.fit();}catch(e){}a.term.focus();}
  termTabsRender();
}
function termClose(id){
  const i=TERMS.findIndex(t=>t.id===id);if(i<0)return;
  const t=TERMS[i];
  try{t.ws.close();}catch(e){}
  try{t.term.dispose();}catch(e){}
  const p=document.getElementById("term-pane-"+id);if(p)p.remove();
  TERMS.splice(i,1);
  if(TERM_ACTIVE===id)TERM_ACTIVE=TERMS.length?TERMS[Math.max(0,i-1)].id:null;
  if(TERM_ACTIVE!==null)termSelect(TERM_ACTIVE);else termTabsRender();
}
function termKill(){while(TERMS.length)termClose(TERMS[0].id);}
async function openTerminal(){
  if(!TERMS.length){await termNew();}
  else if(TERM_ACTIVE!==null)termSelect(TERM_ACTIVE);
  if(!window._termRO){
    window._termRO=new ResizeObserver(()=>{if(CUR!=="terminal")return;
      const a=TERMS.find(t=>t.id===TERM_ACTIVE);if(a)try{a.fit.fit();}catch(e){}});
    window._termRO.observe(document.getElementById("term-host"));
  }
}
// ── Code map: symbol & dependency graph the agents navigate ──────────────────
// One screen, two lenses on the same codebase: Map (symbol graph) | Files
// (the real workspace file browser, previously buried in Settings).
function setCodemapTab(t){
  window._cmTab=t;
  document.getElementById("cmt-map").classList.toggle("on",t==="map");
  document.getElementById("cmt-files").classList.toggle("on",t==="files");
  document.getElementById("codemap-body").hidden=t!=="map";
  const ws=document.getElementById("ws-panel");ws.hidden=t!=="files";
  if(t==="files")loadWorkspace(window._wsPath||"");
}
let CG=null,cgT=null;
async function renderCodeMap(){
  const el=document.getElementById("codemap-body");if(!el)return;
  el.innerHTML='<div class="empty">loading…</div>';
  let d={};try{d=await(await fetch(api("/codegraph"))).json();}catch(e){}
  CG=d;
  if(!d.built){
    el.innerHTML=`<div class="rev-empty"><i class="ti ti-sitemap"></i><div>No code map yet</div><span>Index this project's source into a symbol &amp; dependency map — so agents (and you) can navigate the codebase fast instead of reading it blindly.</span><button class="pri" style="margin-top:16px" onclick="buildCodeMap(this)"><i class="ti ti-refresh"></i> Build code map</button></div>`;return;
  }
  renderCodeMapMain(d);
}
function renderCodeMapMain(d){
  const el=document.getElementById("codemap-body");if(!el)return;
  const langs=Object.entries(d.languages||{}).sort((a,b)=>b[1]-a[1]);
  const langChips=langs.map(([l,n])=>`<span class="cg-lang">${esc(l)} <b>${n}</b></span>`).join("");
  const files=(d.top_files||[]).map(f=>`<div class="cg-file"><div class="cg-fp">${esc(f.path)}</div><div class="cg-fm"><span class="cg-badge">${esc(f.lang)}</span> ${f.symbols} sym · ${f.loc} loc · ${f.imports} imports</div></div>`).join("");
  el.innerHTML=`
   <div class="cg-head">
     <div class="cg-stats"><div><b>${d.files}</b><span>files</span></div><div><b>${d.symbols}</b><span>symbols</span></div><div><b>${d.edges}</b><span>imports</span></div></div>
     <button class="gc-btn" onclick="buildCodeMap(this)"><i class="ti ti-refresh"></i> Rebuild</button>
   </div>
   <div style="font-size:11px;color:var(--dim);margin:4px 0 12px">built ${esc((d.built_at||'').slice(0,16).replace('T',' '))} · agents also read <code style="font-family:ui-monospace,monospace">.coxagent/REPO_MAP.md</code></div>
   <div class="cg-langs">${langChips}</div>
   <div class="sec" style="margin-top:6px">Dependency graph <span style="font-size:11px;color:var(--dim);font-weight:400">· hover highlights · click pins · scroll zooms · drag pans</span></div>
   <div id="cg-hubs" class="cg-hubs"></div>
   <div id="cg-graph" class="cg-graph"><div class="empty" style="padding:20px">loading graph…</div></div>
   <div id="cg-legend" class="cg-legend"></div>
   <div id="cg-info"></div>
   <div class="cg-search" style="margin-top:14px"><i class="ti ti-search"></i><input id="cg-q" placeholder="Search symbols — functions, structs, classes…" oninput="cgSearch(this.value)" autocomplete="off"></div>
   <div id="cg-results"></div>
   <div class="sec" style="margin-top:16px">Top files <span style="font-size:11px;color:var(--dim);font-weight:400">· most symbols first</span></div>
   <div class="cg-files">${files||'<div class="empty">no source files found</div>'}</div>`;
  cgLoadGraph();
}
const CG_LANGCOL={rust:"#dea584",python:"#4b8bbe",typescript:"#3178c6",tsx:"#3178c6",javascript:"#e8d44d",go:"#00add8",java:"#e76f00",csharp:"#68217a",swift:"#f05138",ruby:"#cc342d",c:"#8a8a8a",cpp:"#f34b7d",php:"#777bb4"};
// Stable colour per top-level module (folder) — far more informative than
// per-language colouring in a single-language repo.
const CG_MODPAL=["#38bdf8","#a78bfa","#34d399","#fbbf24","#f472b6","#fb7185","#5eead4","#c4b5fd","#fca5a5","#86efac"];
function cgModule(p){const seg=p.split("/");if(seg.length<2)return "(root)";
  // Generic containers (crates/, packages/…) aren't modules — descend one level.
  const generic=["crates","packages","apps","libs","src","modules","services"];
  return generic.includes(seg[0])&&seg.length>2?seg[0]+"/"+seg[1]:seg[0];}
function cgModColor(mods,m){return CG_MODPAL[mods.indexOf(m)%CG_MODPAL.length];}
async function cgLoadGraph(){
  const box=document.getElementById("cg-graph");if(!box)return;
  let d={};try{d=await(await fetch(api("/codegraph/deps"))).json();}catch(e){}
  if(!d.nodes||!d.nodes.length){box.innerHTML='<div class="empty" style="padding:20px;font-size:12px">no import relationships detected between files</div>';return;}
  // Hubs: the most-connected files — the heart of the codebase.
  const degAll={};(d.links||[]).forEach(l=>{degAll[l.source]=(degAll[l.source]||0)+1;degAll[l.target]=(degAll[l.target]||0)+1;});
  const hubs=Object.entries(degAll).sort((a,b)=>b[1]-a[1]).slice(0,6);
  const hubEl=document.getElementById("cg-hubs");
  if(hubEl)hubEl.innerHTML=hubs.map(([f,n])=>`<button class="cg-hub" onclick="cgFocusNode('${esc(f)}')" title="${esc(f)}"><i class="ti ti-flame" style="color:var(--amber)"></i> ${esc(f.split("/").slice(-1)[0])} <b>${n}</b></button>`).join("");
  cgRenderGraph(box,d.nodes,d.links||[]);
}
function cgFocusNode(path){
  const box=document.getElementById("cg-graph");if(!box)return;
  const g=[...box.querySelectorAll(".cgn")].find(n=>n.dataset.path===path);
  if(g){g.dispatchEvent(new Event("mouseenter"));box.scrollIntoView({behavior:"smooth",block:"center"});
    setTimeout(()=>g.dispatchEvent(new Event("mouseleave")),2500);}
}
// Tiny 3D force layout (no libs): repulsion + link springs in 3D, projected
// with perspective onto the SVG — an auto-rotating hologram. Drag rotates,
// wheel zooms, hover/click still highlight & pin.
function cgRenderGraph(box,rawNodes,rawLinks){
  const W=800,H=460,cx=W/2,cy=H/2,F=520,R=200;
  // Limit to top 90 most-connected nodes — prevents visual clutter.
  const MAX_NODES=90;
  const deg={};
  rawLinks.forEach(l=>{deg[l.source]=(deg[l.source]||0)+1;deg[l.target]=(deg[l.target]||0)+1;});
  const keepIds=new Set(
    rawNodes.map(n=>[n.id,(deg[n.id]||0)]).sort((a,b)=>b[1]-a[1]).slice(0,MAX_NODES).map(x=>x[0])
  );
  const nodes=rawNodes.filter(n=>keepIds.has(n.id)).map(n=>({...n,
    x:(Math.random()-.5)*R*1.6,y:(Math.random()-.5)*R*1.2,z:(Math.random()-.5)*R*1.6,
    vx:0,vy:0,vz:0,ph:Math.random()*Math.PI*2,sp:.4+Math.random()*.5}));
  const idx={};nodes.forEach((n,i)=>idx[n.id]=i);
  const links=rawLinks.filter(l=>idx[l.source]!=null&&idx[l.target]!=null).map(l=>({s:idx[l.source],t:idx[l.target]}));
  for(let it=0;it<260;it++){
    const k=it<180?1:.4;
    for(let i=0;i<nodes.length;i++){let fx=0,fy=0,fz=0;
      for(let j=0;j<nodes.length;j++){if(i===j)continue;
        let dx=nodes[i].x-nodes[j].x,dy=nodes[i].y-nodes[j].y,dz=nodes[i].z-nodes[j].z;
        let d2=dx*dx+dy*dy+dz*dz+.01;let f=5200/d2;fx+=dx*f;fy+=dy*f;fz+=dz*f;}
      fx-=nodes[i].x*.02;fy-=nodes[i].y*.028;fz-=nodes[i].z*.02;
      nodes[i].vx=(nodes[i].vx+fx*k)*.8;nodes[i].vy=(nodes[i].vy+fy*k)*.8;nodes[i].vz=(nodes[i].vz+fz*k)*.8;}
    for(const l of links){const a=nodes[l.s],b=nodes[l.t];
      let dx=b.x-a.x,dy=b.y-a.y,dz=b.z-a.z,dist=Math.sqrt(dx*dx+dy*dy+dz*dz)||1;
      const f=(dist-120)*.02*k,ux=dx/dist*f,uy=dy/dist*f,uz=dz/dist*f;
      a.vx+=ux;a.vy+=uy;a.vz+=uz;b.vx-=ux;b.vy-=uy;b.vz-=uz;}
    for(const n of nodes){n.x+=n.vx;n.y+=n.vy;n.z+=n.vz;
      const m=Math.sqrt(n.x*n.x+n.y*n.y+n.z*n.z);if(m>R){const s=R/m;n.x*=s;n.y*=s;n.z*=s;}}
  }
  const short=p=>p.split("/").slice(-1)[0];
  // Colour by top-level module: structure jumps out even in a one-language repo.
  const mods=[...new Set(nodes.map(n=>cgModule(n.id)))].sort();
  // Static markup; the per-frame projection drives transform/opacity/order.
  const line=links.map(l=>`<path class="cge" data-s="${l.s}" data-t="${l.t}" d=""/>`).join("");
  const circ=nodes.map((n,i)=>{const dg=deg[n.id]||0;const r=Math.max(5,Math.min(15,5+Math.sqrt(dg)*2.2));const col=cgModColor(mods,cgModule(n.id));
    const hub=dg>=8,showLabel=dg>=2;
    return `<g class="cgn" data-i="${i}" data-path="${esc(n.id)}"><title>${esc(n.id)} · ${n.symbols} symbols · ${dg} links</title>
      <circle class="cg-ring" r="${(r+4).toFixed(1)}" style="stroke:${col}"/>
      ${hub?`<circle class="cg-pulse" r="${(r+4).toFixed(1)}" style="stroke:${col}"/>`:''}
      <circle class="cg-core" r="${r.toFixed(1)}" fill="${col}" filter="url(#cgGlow)"/>
      ${showLabel?`<text x="${(r+6).toFixed(1)}" y="3" class="cgl">${esc(short(n.id))}</text>`:''}</g>`;}).join("");
  box.innerHTML=`<svg viewBox="0 0 ${W} ${H}" class="cg-svg"><defs>
    <filter id="cgGlow" x="-60%" y="-60%" width="220%" height="220%"><feGaussianBlur stdDeviation="2.6" result="b"/><feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge></filter>
    <linearGradient id="cgHot" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#22d3ee"/><stop offset="1" stop-color="#a78bfa"/></linearGradient>
  </defs><g class="cg-view"><g class="cg-edges">${line}</g><g class="cg-nodes">${circ}</g></g></svg>`;
  const gEls=[...box.querySelectorAll(".cgn")],pEls=[...box.querySelectorAll(".cge")];
  const nodesG=box.querySelector(".cg-nodes");
  // Camera: slow auto-spin, drag to rotate, wheel to zoom.
  let yaw=0,pitch=.28,zoom=1,drag=null,frame=0;
  const SPIN=.1; // rad/s
  if(window._cgRaf)cancelAnimationFrame(window._cgRaf);
  const t0=performance.now();let tPrev=t0;
  const tick=t=>{if(!box.isConnected){window._cgRaf=null;return;}
    const s=(t-t0)/1000,dt=Math.min(.05,(t-tPrev)/1000);tPrev=t;
    if(!drag)yaw+=SPIN*dt;
    const cY=Math.cos(yaw),sY=Math.sin(yaw),cP=Math.cos(pitch),sP=Math.sin(pitch);
    nodes.forEach((n,i)=>{
      // gentle 3D breathing per node
      const bx=n.x+Math.sin(s*n.sp+n.ph)*3,by=n.y+Math.cos(s*n.sp*.8+n.ph)*3,bz=n.z+Math.sin(s*n.sp*.6+n.ph*2)*3;
      const x1=bx*cY+bz*sY,z1=bz*cY-bx*sY;
      const y2=by*cP-z1*sP,z2=z1*cP+by*sP;
      const pr=F/(F+z2)*zoom;
      n.px=cx+x1*pr;n.py=cy+y2*pr;n.pz=z2;
      gEls[i].setAttribute("transform",`translate(${n.px.toFixed(1)},${n.py.toFixed(1)}) scale(${pr.toFixed(3)})`);
      gEls[i].style.opacity=(.3+.7*Math.max(0,Math.min(1,(R-z2)/(2*R)))).toFixed(2);
    });
    pEls.forEach((p,j)=>{const a=nodes[links[j].s],b=nodes[links[j].t];
      const mx=(a.px+b.px)/2,my=(a.py+b.py)/2,dx=b.px-a.px,dy=b.py-a.py;
      p.setAttribute("d",`M${a.px.toFixed(1)},${a.py.toFixed(1)} Q${(mx-dy*.1).toFixed(1)},${(my+dx*.1).toFixed(1)} ${b.px.toFixed(1)},${b.py.toFixed(1)}`);
      p.style.opacity=(.25+.75*Math.max(0,Math.min(1,(R-(a.pz+b.pz)/2)/(2*R)))).toFixed(2);});
    // Painter's order: far nodes behind near ones (re-sorted sparsely).
    if(++frame%12===0)[...gEls].sort((a,b)=>nodes[+b.dataset.i].pz-nodes[+a.dataset.i].pz).forEach(g=>nodesG.appendChild(g));
    window._cgRaf=requestAnimationFrame(tick);};
  window._cgRaf=requestAnimationFrame(tick);
  const legend=document.getElementById("cg-legend");
  if(legend)legend.innerHTML=mods.map(m=>`<span class="cg-leg"><span class="sdot" style="background:${cgModColor(mods,m)}"></span>${esc(m)}</span>`).join("");
  // Hover highlights; click pins the highlight + shows an info card.
  let pinned=-1;
  const highlight=i=>{const nb=new Set([i]);
    box.querySelectorAll(".cge").forEach(e=>{const s=+e.dataset.s,t=+e.dataset.t;const on=s===i||t===i;e.classList.toggle("hot",on);if(on){nb.add(s);nb.add(t);}});
    box.querySelectorAll(".cgn").forEach(n=>n.classList.toggle("dim",!nb.has(+n.dataset.i)));};
  const clear=()=>{box.querySelectorAll(".cge").forEach(e=>e.classList.remove("hot"));box.querySelectorAll(".cgn").forEach(n=>n.classList.remove("dim"));};
  const info=document.getElementById("cg-info");
  const showInfo=i=>{const n=nodes[i];if(!info)return;
    const ins=links.filter(l=>l.t===i).length,outs=links.filter(l=>l.s===i).length;
    info.innerHTML=`<div class="cg-card"><div class="cg-fp" style="font-weight:700">${esc(n.id)}</div>
      <div class="cg-fm">${n.symbols||0} symbols · <span style="color:var(--green)">${ins} imported-by</span> · <span style="color:var(--accent2)">${outs} imports</span> · module <b>${esc(cgModule(n.id))}</b></div>
      <div style="display:flex;gap:8px;margin-top:8px">
        <button class="gc-btn" onclick="openFile('${esc(n.id)}')"><i class="ti ti-file-code"></i> Preview</button>
        <button class="gc-btn" onclick="cgSearchFileSyms('${esc(short(n.id))}')"><i class="ti ti-search"></i> Symbols</button>
        <button class="gc-btn" onclick="document.getElementById('cg-info').innerHTML='';"><i class="ti ti-x"></i></button>
      </div></div>`;};
  box.querySelectorAll(".cgn").forEach(g=>{
    g.addEventListener("mouseenter",()=>{if(pinned<0)highlight(+g.dataset.i);});
    g.addEventListener("mouseleave",()=>{if(pinned<0)clear();});
    g.addEventListener("click",e=>{e.stopPropagation();if(drag&&drag.moved)return;const i=+g.dataset.i;
      if(pinned===i){pinned=-1;clear();if(info)info.innerHTML="";return;}
      pinned=i;highlight(i);showInfo(i);});
  });
  // Drag rotates the hologram (not pan); wheel zooms the projection.
  const svg=box.querySelector(".cg-svg");
  svg.addEventListener("wheel",e=>{e.preventDefault();
    zoom=Math.max(.5,Math.min(3.5,zoom*(e.deltaY<0?1.1:.9)));},{passive:false});
  svg.addEventListener("mousedown",e=>{drag={x:e.clientX,y:e.clientY,moved:false};});
  window.addEventListener("mousemove",e=>{if(!drag)return;
    const dx=e.clientX-drag.x,dy=e.clientY-drag.y;drag.x=e.clientX;drag.y=e.clientY;
    if(Math.abs(dx)+Math.abs(dy)>1)drag.moved=true;
    yaw+=dx*.006;pitch=Math.max(-1.1,Math.min(1.1,pitch+dy*.004));});
  // Clear drag on the next tick so the click handler can still see drag.moved.
  window.addEventListener("mouseup",()=>{setTimeout(()=>{drag=null;},0);});
  svg.addEventListener("click",()=>{if(drag&&drag.moved)return;
    if(pinned>=0){pinned=-1;clear();if(info)info.innerHTML="";}});
}
// Search box helper: prefill with a file's stem to list its symbols.
function cgSearchFileSyms(stem){const q=document.getElementById("cg-q");if(!q)return;
  q.value=stem.replace(/\.[a-z]+$/,"");q.focus();cgSearch(q.value);}
async function buildCodeMap(btn){const o=btn.innerHTML;btn.innerHTML='<i class="ti ti-loader-2 att-spin"></i> Indexing…';btn.disabled=true;
  try{const d=await(await fetch(api("/codegraph/build"),{method:"POST"})).json();CG=d;renderCodeMapMain(d);toasty("Code map built — "+d.files+" files, "+d.symbols+" symbols","ok");}
  catch(e){toasty("Build failed","err");btn.innerHTML=o;btn.disabled=false;}
}
function cgSearch(q){clearTimeout(cgT);cgT=setTimeout(async()=>{
  const box=document.getElementById("cg-results");if(!box)return;
  if(!q.trim()){box.innerHTML="";return;}
  let d={};try{d=await(await fetch(api("/codegraph?q="+encodeURIComponent(q)))).json();}catch(e){}
  const r=(d&&d.results)||[];
  box.innerHTML=r.length?`<div class="cg-syms">`+r.map(s=>`<div class="cg-sym" onclick="cgImpact('${esc(s.name)}')" title="See where ${esc(s.name)} is used"><span class="cg-k cg-k-${esc(s.kind)}">${esc(s.kind)}</span><b>${s.scope?`<span style="color:var(--dim);font-weight:500">${esc(s.scope)}::</span>`:''}${esc(s.name)}</b><span class="cg-loc">${esc(s.file)}:${s.line}</span><i class="ti ti-arrow-right" style="margin-left:6px;color:var(--dim);font-size:13px"></i></div>`).join("")+`</div>`:'<div class="empty" style="padding:10px;font-size:12px">no symbols match</div>';
},250);}
// Impact analysis: where is this symbol used across the tree?
async function cgImpact(name){
  const body=document.getElementById("cgimpact-body");
  body.innerHTML='<div class="empty" style="padding:24px">scanning usages…</div>';
  document.getElementById("ov-cgimpact").classList.add("open");
  let d={};try{d=await(await fetch(api("/codegraph/refs?name="+encodeURIComponent(name)))).json();}catch(e){}
  const refs=(d&&d.refs)||[];
  if(!refs.length){body.innerHTML=`<h3><code>${esc(name)}</code></h3><div class="empty" style="padding:16px">no usages found</div>`;return;}
  // Group by file, definitions first.
  const byFile={};refs.forEach(r=>{(byFile[r.file]=byFile[r.file]||[]).push(r);});
  const files=Object.keys(byFile).sort((a,b)=>{const da=byFile[a].some(r=>r.is_def),db=byFile[b].some(r=>r.is_def);return (db-da)||a.localeCompare(b);});
  const esc2=t=>esc(t).replace(new RegExp("("+name.replace(/[.*+?^${}()|[\]\\]/g,"\\$&")+")","g"),'<mark>$1</mark>');
  // Call graph: who calls this fn (impact) and what it calls.
  const callers=(d.callers||[]),callees=(d.callees||[]);
  const callList=(arr,dir)=>arr.length?`<div class="cgi-file"><div class="cgi-fp"><i class="ti ti-${dir==='in'?'arrow-down-left':'arrow-up-right'}" style="font-size:13px;color:var(--accent2)"></i> ${dir==='in'?'Called by':'Calls'} <span style="color:var(--dim)">· ${arr.length}</span></div>`+
    arr.map(c=>`<div class="cgi-ref"><span class="cgi-ln">${c.line}</span><code>${esc(c.label)}</code><span class="cgi-cf">${esc(c.file.split('/').slice(-1)[0])}</span></div>`).join("")+`</div>`:'';
  const callGraph=(callers.length||callees.length)?`<div class="sec" style="margin:14px 0 6px;font-size:12px">Call graph</div><div class="cgi-list">${callList(callers,'in')}${callList(callees,'out')}</div>`:'';
  body.innerHTML=`<h3 style="display:flex;align-items:center;gap:8px"><i class="ti ti-sitemap" style="color:var(--accent2)"></i> <code style="font-family:ui-monospace,monospace">${esc(name)}</code></h3>
    <div class="msub">${d.uses} usage${d.uses===1?'':'s'} · ${d.defs} definition${d.defs===1?'':'s'} across ${files.length} file${files.length===1?'':'s'}${callers.length?` · called by ${callers.length}`:''}</div>
    ${callGraph}
    <div class="sec" style="margin:14px 0 6px;font-size:12px">All references</div>
    <div class="cgi-list">`+files.map(f=>`
      <div class="cgi-file"><div class="cgi-fp"><i class="ti ti-file-code" style="font-size:13px"></i> ${esc(f)} <span style="color:var(--dim)">· ${byFile[f].length}</span></div>`+
      byFile[f].map(r=>`<div class="cgi-ref${r.is_def?' def':''}"><span class="cgi-ln">${r.line}</span>${r.is_def?'<span class="cgi-badge">def</span>':''}<code>${esc2(r.text)}</code></div>`).join("")+`</div>`).join("")+`</div>`;
}
async function createUser(){
  const username=document.getElementById("usr-name").value.trim();
  const password=document.getElementById("usr-pass").value;
  const role=document.getElementById("usr-role").value;
  const name=document.getElementById("usr-fullname").value.trim();
  const email=document.getElementById("usr-email").value.trim();
  const projects=[...document.querySelectorAll("#usr-projpick input:checked")].map(c=>c.value);
  const err=document.getElementById("usr-err");
  if(!username||!password){err.textContent="Username and password required.";return;}
  err.textContent="";
  try{
    const r=await fetch("/api/auth/users",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({username,password,role,name,email,projects})});
    if(!r.ok){err.textContent=await r.text()||"Failed.";return;}
    ["usr-name","usr-pass","usr-fullname","usr-email"].forEach(id=>document.getElementById(id).value="");
    document.querySelectorAll("#usr-projpick input:checked").forEach(c=>c.checked=false);
    close_("ov-invite");toasty("Member added","ok");
    if(isManageMode()){MG=null;renderManage();}else{renderUsers();}
  }catch(e){err.textContent="Network error.";}
}
let EU_USER=null;
function editUser(un){const username=decodeURIComponent(un);const u=USERS.find(x=>x.username===username);if(!u)return;
  EU_USER=username;
  document.getElementById("eu-title").textContent=username;
  document.getElementById("eu-name").value=u.name||"";
  document.getElementById("eu-email").value=u.email||"";
  document.getElementById("eu-role").innerHTML=roleOptions(u.role||"ba");
  document.getElementById("eu-pass").value="";
  document.getElementById("eu-msg").textContent="";
  document.getElementById("ov-user").classList.add("open");
}
async function saveUser(){if(!EU_USER)return;
  const name=document.getElementById("eu-name").value.trim();
  const email=document.getElementById("eu-email").value.trim();
  const role=document.getElementById("eu-role").value;
  try{const r=await fetch("/api/auth/users/"+encodeURIComponent(EU_USER),{method:"PATCH",headers:{"Content-Type":"application/json"},body:JSON.stringify({name,email,role})});
    if(r.ok){close_("ov-user");toasty("User updated","ok");renderUsers();}
    else{document.getElementById("eu-msg").textContent=await r.text()||"Failed.";}
  }catch(e){document.getElementById("eu-msg").textContent="Network error.";}
}
async function resetUserPassword(){if(!EU_USER)return;
  const pass=document.getElementById("eu-pass").value;
  const msg=document.getElementById("eu-msg");
  if(pass.length<4){msg.textContent="Password too short (min 4).";return;}
  try{const r=await fetch("/api/auth/users/"+encodeURIComponent(EU_USER)+"/password",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({password:pass})});
    if(r.ok){document.getElementById("eu-pass").value="";msg.style.color="var(--green)";msg.textContent="Password reset ✓";}
    else{msg.style.color="var(--red)";msg.textContent=await r.text()||"Failed.";}
  }catch(e){msg.style.color="var(--red)";msg.textContent="Network error.";}
}
async function deleteUser(username){if(!await coxModal({title:"Remove account",message:"Xoá tài khoản \""+username+"\"? Không hoàn tác được.",danger:true,confirmText:"Remove"}))return;try{const r=await fetch("/api/auth/users/"+username,{method:"DELETE"});if(!r.ok)document.getElementById("usr-err").textContent=await r.text();renderUsers();}catch(e){}}
async function assignUser(pid,username){try{await fetch("/api/projects/"+pid+"/members",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({username:decodeURIComponent(username)})});renderUsers();}catch(e){}}
async function unassignUser(pid,username){try{await fetch("/api/projects/"+pid+"/members/"+username,{method:"DELETE"});renderUsers();}catch(e){}}
function fmtBytes(n){return n<1024?n+" B":n<1048576?(n/1024).toFixed(1)+" KB":(n/1048576).toFixed(1)+" MB";}
function renderTranscripts(){
  const el=document.getElementById("transcript-list");if(!el)return;
  fetch(api("/transcripts")).then(r=>r.ok?r.json():[]).then(rows=>{
    if(!Array.isArray(rows)||!rows.length){el.innerHTML='<div class="empty">no transcripts yet — they appear once the loop runs an engine</div>';return;}
    el.innerHTML=rows.map(t=>{const m=t.name.match(/-(\w[\w-]*)\.md$/);const role=m?m[1]:"run";const col=cvar(AC[role.toUpperCase().replace("_","-")]||"--muted");
      return `<div class="act" onclick="openTranscript('${encodeURIComponent(t.name)}','${esc(t.name)}')" style="cursor:pointer">
        <div class="ad" style="background:${col}22;color:${col}"><i class="ti ti-file-text" style="font-size:13px"></i></div>
        <div class="atx"><span class="who">${esc(t.name)}</span></div><span class="tm">${fmtBytes(t.size)}</span></div>`;}).join("");
  }).catch(()=>{el.innerHTML='<div class="empty">unable to load transcripts</div>';});
}
function deviceIcon(label){const l=(label||"").toLowerCase();
  const os=l.includes("iphone")||l.includes("android")?"device-mobile":(l.includes("ipad")?"device-tablet":null);
  if(os)return os;
  if(l.includes("chrome"))return "brand-chrome";if(l.includes("firefox"))return "brand-firefox";
  if(l.includes("safari"))return "brand-safari";if(l.includes("edge"))return "brand-edge";
  return "device-desktop";}
async function renderSessions(){
  const el=document.getElementById("team-sessions");if(!el)return;
  let ss=[];try{ss=await(await fetch("/api/auth/sessions")).json();}catch(e){}
  if(!Array.isArray(ss)||!ss.length){el.innerHTML="";return;}
  // Only show current device — not history.
  var cur=ss.find(function(s){return s.current;});
  if(!cur){el.innerHTML="";return;}
  el.innerHTML=`<div class="sec" style="margin-top:0">Your device</div>
    <div class="panel" style="margin-top:6px">
      <div class="sessrow">
        <div class="sessic"><i class="ti ti-${deviceIcon(cur.label)}"></i></div>
        <div style="flex:1;min-width:0"><div class="sessname">${esc(cur.label)} <span class="pbadge on">this device</span></div>
          <div class="ameta">signed in ${relTime(cur.at)}</div>
        </div>
      </div>
    </div>`;
}
async function loadAgentEvals(){
  const el=document.getElementById("team-evals");if(!el)return;
  try{
    const d=await(await fetch(api("/agent-evals"))).json();
    const kpi=(label,val,icon,color)=>`<div class="kpi"><div class="kv" style="color:${color||'var(--text)'}">${val}</div><div class="kl"><i class="ti ti-${icon}"></i> ${label}</div></div>`;
    const churn=+d.churn_per_ship;
    el.innerHTML=
      kpi("shipped total",d.shipped_total,"rocket")+
      kpi("shipped · 7d",d.shipped_7d,"calendar-week")+
      kpi("cost / ship",d.cost_per_ship_usd>0?"$"+(+d.cost_per_ship_usd).toFixed(2):"—","coin")+
      kpi("retry churn / ship",churn.toFixed(2),"refresh-alert",churn>1.5?"var(--red)":churn>0.5?"var(--amber)":"var(--green)")+
      kpi("parked tickets",d.parked,"hand-stop",d.parked>0?"var(--amber)":"var(--green)")+
      kpi("PRs stuck",d.prs_stuck,"git-pull-request",d.prs_stuck>0?"var(--red)":"var(--green)")+
      (d.per_role||[]).filter(r=>r.runs>0).map(r=>kpi(r.role+" avg/run","$"+(+r.avg_cost_usd).toFixed(2),"robot")).join("");
  }catch(e){el.innerHTML='<div class="empty">evals unavailable</div>';}
}
async function renderTeamPeople(){
  const el=document.getElementById("team-people");if(!el)return;
  let rows=[];try{rows=await(await fetch(api("/members"))).json();}catch(e){}
  if(!Array.isArray(rows)){el.innerHTML='<div class="empty">admin only</div>';return;}
  const members=rows.filter(u=>u.assigned);
  const isAdmin=r=>String(r).toLowerCase()==="admin";
  // Read-only here — assignment now lives in Users.
  const memHtml=members.length?members.map(u=>{const grad=isAdmin(u.role)?"linear-gradient(135deg,var(--amber),#b45309)":"linear-gradient(135deg,var(--accent),var(--accent2))";
    return `<div class="arow"><div class="aav" style="background:${grad}">${esc((u.username||'?').slice(0,2).toUpperCase())}</div>
      <div style="flex:1;min-width:0"><div class="aname">${esc(u.username)}</div><div class="ameta">on this project · <a onclick="nav('people')" style="color:var(--accent2);cursor:pointer">activity</a></div></div>
      <span class="arole ${isAdmin(u.role)?'admin':'viewer'}">${esc(roleLabel(u.role))}</span></div>`;}).join(""):'<div class="empty">no humans assigned yet</div>';
  el.innerHTML=`<div class="alist">${memHtml}</div>
    <div style="font-size:11px;color:var(--dim);margin-top:10px">Manage who works on which project in <a onclick="nav('access')" style="color:var(--accent2);cursor:pointer">Users</a>.</div>`;
}
async function assignMember(){const s=document.getElementById("member-add");if(!s||!s.value)return;
  try{await fetch(api("/members"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({username:s.value})});renderTeamPeople();}catch(e){}}
async function unassignMember(u){try{await fetch(api("/members/"+u),{method:"DELETE"});renderTeamPeople();}catch(e){}}
// Flag tickets that share a normalised title — likely duplicate work.
// The warning clears automatically once the duplicate is gone (a ticket rejected
// or its title changed), or the user can dismiss a specific pair manually.
let DUP_DISMISSED=(()=>{try{return new Set(JSON.parse(localStorage.getItem("cox_dupx")||"[]"));}catch(e){return new Set();}})();
function dismissDup(key){DUP_DISMISSED.add(key);localStorage.setItem("cox_dupx",JSON.stringify([...DUP_DISMISSED]));renderDupWarn(STATE);}
function renderDupWarn(s){
  const el=document.getElementById("team-dupwarn");if(!el)return;
  const norm=t=>(t.title||"").toLowerCase().replace(/[^a-z0-9]+/g," ").trim();
  // Only warn about duplicates that can still be cleaned up (not yet built).
  // Already-shipped/in-progress dupes are water under the bridge — don't nag.
  const actionable=t=>["pending","ready","open"].includes(t.status);
  const g={};(s.tickets||[]).filter(actionable).forEach(t=>{const k=norm(t);if(k)(g[k]=g[k]||[]).push(t);});
  // Key a group by its members so a dismissal only sticks while that exact set persists.
  const dups=Object.values(g).filter(x=>x.length>1)
    .map(x=>({x,key:x.map(t=>t.id).sort().join("+")}))
    .filter(d=>!DUP_DISMISSED.has(d.key));
  el.innerHTML=dups.length?dups.map(({x,key})=>`<div class="dupwarn"><i class="ti ti-copy-check"></i><div><b>Possible duplicate work:</b> ${x.map(t=>`<span class="tk" style="cursor:pointer" onclick="showTicket('${t.id}')">${esc(t.id)}</span>`).join(" ")} share the title “${esc(x[0].title)}”.</div><button class="dup-dismiss" title="Dismiss this warning" onclick="dismissDup('${key}')"><i class="ti ti-x"></i></button></div>`).join(""):"";
}

// Cross-machine "teams online": every runner (account@host) working this project
// right now, from the shared worker registry — so one dashboard shows them all.
async function renderTeamsOnline(){
  const el=document.getElementById("team-online");if(!el)return;
  let ws=[];try{ws=await (await fetch(api("/workers"))).json();}catch(e){return;}
  window.WORKERS=Array.isArray(ws)?ws:[];
  if(!Array.isArray(ws)||!ws.length){el.innerHTML="";return;}
  el.innerHTML=`<div class="teamson"><div class="tso-h"><i class="ti ti-server-bolt"></i> Teams online <span>${ws.length}</span></div>`
    +`<div class="tso-list">`+ws.map(w=>{
      // Show what each team is doing right now: the live agent role (SA/DEV…),
      // or leader/worker between phases. Highlight an actual running agent.
      const role=(w.role||'idle');
      const busy=!/^(leader|worker|idle)$/i.test(role);
      // Only show Stop on your OWN operator (or all, if you're an admin) — each
      // user controls only their own team.
      const acct=(w.worker||'').split('@')[0].toLowerCase();
      const mine=!ME||(ME.role==="admin")||((ME.username||'').toLowerCase()===acct);
      const stopBtn=mine?`<button class="tso-stop" title="Stop this operator (idles it — saves its tokens)" onclick="stopOperator('${esc(w.worker)}')"><i class="ti ti-player-stop"></i></button>`:'';
      return `<div class="tso"><span class="tso-dot"></span><span class="tso-id">${esc(w.worker)}</span>`
        +`<span class="tso-role ${busy?'lead':''}">${esc(role.replace(/_/g,'-').toUpperCase())}</span>`
        +(w.ticket?`<span class="tso-tk">${esc(w.ticket)}</span>`:'')
        +stopBtn
        +`</div>`;
    }).join("")+`</div></div>`;
}
async function stopOperator(op){
  if(!await coxModal({title:"Stop operator",message:"Dừng operator "+op+"? Nó sẽ idle (không đốt token) cho tới khi được start lại.",confirmText:"Stop"}))return;
  try{await fetch(api("/operators/"+encodeURIComponent(op)+"/stop"),{method:"POST"});}catch(e){}
  setTimeout(renderTeamsOnline,600);
}
async function openAgent(role,worker){
  const meta=AGENTS.find(a=>a[0]===role);const col=cvar((meta&&meta[2])||"--muted");
  AGENT_LOG_WORKER=worker||"";
  // If several operators run this role, offer a picker to view each one's log.
  const runners=(window.WORKERS||[]).filter(w=>(w.role||"").replace(/_/g,"-").toUpperCase()===role);
  const who=AGENT_LOG_WORKER?AGENT_LOG_WORKER.split('@')[0]:(runners[0]&&runners[0].worker?runners[0].worker.split('@')[0]:"");
  if(AGENT_LOG_WORKER==="" && runners[0])AGENT_LOG_WORKER=runners[0].worker;
  const tk=(runners.find(r=>r.worker===AGENT_LOG_WORKER)||runners[0]||{}).ticket||"";
  document.getElementById("agent-title").innerHTML=`<span class="av" style="width:30px;height:30px;background:${col}22;color:${col}">${esc(initials(role))}</span> ${esc(role)}${who?` <span style="font-size:12px;color:var(--muted)">· <i class="ti ti-user-cog"></i> ${esc(who)}</span>`:''}${tk?` <span class="tid">${esc(tk)}</span>`:''}`;
  document.getElementById("agent-sub").innerHTML=(meta&&meta[1])||"agent";
  // Operator picker chips when >1 operator is on this role.
  const pick=document.getElementById("agent-picker");
  if(pick){
    pick.innerHTML=runners.length>1?runners.map(r=>{
      const acct=r.worker.split('@')[0];const on=r.worker===AGENT_LOG_WORKER;
      return `<button class="agpick ${on?'on':''}" onclick="openAgent('${role}','${esc(r.worker)}')"><i class="ti ti-user-cog"></i> ${esc(acct)}${r.ticket?` · ${esc(r.ticket)}`:''}</button>`;
    }).join(""):"";
  }
  const acts=(STATE.activity||[]).filter(a=>a.agent===role).slice(-15).reverse();
  document.getElementById("agent-acts").innerHTML=acts.length?acts.map(a=>
    `<div class="act"><div class="atx"><span>${esc(a.action)}</span> ${a.ticket?`<span class="tk">${esc(a.ticket)}</span>`:''}</div><span class="tm">${esc((a.at||'').slice(11,16))}</span></div>`).join("")
    :'<div class="empty">no recorded actions yet</div>';
  const body=document.getElementById("agent-transcript");
  body.innerHTML='<div class="wl-empty"><i class="ti ti-loader-2"></i> loading…</div>';
  AGENT_LOG_LAST=null;   // force a fresh render for this role/operator
  document.getElementById("ov-agent").classList.add("open");
  AGENT_LOG_ROLE=role.toLowerCase().replace(/-/g,"_");  // serde key: DEV-FEATURE→dev_feature
  await pollAgentLog();               // first fetch now
  clearInterval(AGENT_LOG_TIMER);
  AGENT_LOG_TIMER=setInterval(pollAgentLog,1500);  // then live every 1.5s
}
let AGENT_LOG_TIMER=null, AGENT_LOG_ROLE=null, AGENT_LOG_WORKER="", AGENT_LOG_LAST=null;
// Icon + colour for a tool name, so every engine's tool calls read at a glance.
// A tool call, said in words: "Read run_chat_reply.rs:400-500" instead of a
// truncated JSON blob. Long absolute paths collapse to the part a reader
// recognises — the file, and where it sits in the repo.
function wlShortPath(p){
  const s=String(p||"").replace(/^.*?\/codebase\//,"").replace(/^.*?\.claude\/worktrees\/[^/]+\//,"");
  const parts=s.split("/");
  return parts.length>3?parts.slice(-3).join("/"):s;
}
function wlSay(name,args){
  const n=String(name||"").toLowerCase().replace(/^mcp__[^_]*__/,"");
  const raw=String(args||"");
  let a={};
  try{a=JSON.parse(raw||"{}");}catch(e){
    // The log line is often TRUNCATED mid-JSON, which used to leave a bare
    // "Run" with no command. Salvage the value we care about by hand.
    const grab=k=>{const m=raw.match(new RegExp('"'+k+'"\\s*:\\s*"((?:[^"\\\\]|\\\\.)*)'));return m?m[1].replace(/\\"/g,'"').replace(/\\n/g," "):"";};
    a={command:grab("command"),file_path:grab("file_path"),pattern:grab("pattern"),path:grab("path"),url:grab("url"),description:grab("description")};
  }
  const f=a.file_path||a.path||a.notebook_path;
  const where=f?wlShortPath(f):"";
  const span=(a.offset!=null)?`:${a.offset}${a.limit?"-"+(a.offset+a.limit):""}`:"";
  if(n==="read")return {verb:"Read",detail:where?where+span:""};
  if(n==="edit"||n==="multiedit")return {verb:"Edit",detail:where};
  if(n==="write")return {verb:"Write",detail:where};
  if(n==="bash"){
    let c=String(a.command||"").replace(/\s+/g," ").trim();
    if(!c)c=raw.replace(/^\{|\}$/g,"").replace(/\s+/g," ").slice(0,92);
    // Strip the boilerplate that fronts almost every agent command — a
    // `cd <worktree> &&` prefix and absolute tool paths — so the meaningful
    // part shows: "cargo test -p …" not "cd /tmp/pr-review-b043 && /opt/…".
    c=c.replace(/^cd\s+\S+\s*&&\s*/,"").replace(/\/\S*\/(cargo|npm|npx|node|git|python3?|sed|grep|rg)\b/g,"$1");
    return {verb:"Run",detail:c.length>92?c.slice(0,92)+"…":(c||"(command not logged)")};
  }
  if(n==="grep")return {verb:"Search",detail:[a.pattern,a.path?"in "+wlShortPath(a.path):""].filter(Boolean).join(" ")};
  if(n==="glob")return {verb:"Find files",detail:a.pattern||""};
  if(n==="webfetch")return {verb:"Fetch",detail:a.url||""};
  if(n==="task"||n==="agent")return {verb:"Delegate",detail:a.description||""};
  if(n==="todowrite")return {verb:"Update plan",detail:""};
  if(n==="reportfindings"||n==="structuredoutput"){
    const v=a.decision||a.verdict||"";
    return {verb:"Review verdict",detail:v?String(v).replace(/_/g," "):""};
  }
  // Unknown tool: keep the name, show the first meaningful argument.
  const first=Object.entries(a).find(([,v])=>typeof v==="string"&&v.trim());
  return {verb:name,detail:first?String(first[1]).slice(0,80):""};
}

// Plain-language labels for the agent-harness bookkeeping tools, so a log
// reader doesn't mistake normal scheduling for a failure.
function wlHarnessNote(name,args){
  const n=(name||"").toLowerCase();
  const a=String(args||"");
  if(n==="schedulewakeup"){
    if(a.includes('"stop"'))return "cancelled its own wake-up timer (not an error — the task will notify by itself)";
    return "set a wake-up timer to check back later";
  }
  if(n==="taskstop")return "stopped one of its background tasks";
  if(n==="croncreate"||n==="crondelete")return "adjusted its own schedule";
  return null;
}

function wlToolMeta(name){
  const n=(name||"").toLowerCase().replace(/^mcp__[^_]*__/,"");  // strip mcp__server__ prefix
  const rules=[
    [/(^|_)(read|view|cat|open|file_?text)/,"ti-file-text","--blue"],
    [/(edit|write|patch|create|apply|multiedit)/,"ti-pencil","--amber"],
    [/(bash|shell|exec|run|cmd|command|terminal)/,"ti-terminal-2","--green"],
    [/(grep|search|glob|find|ripgrep|query|symbol)/,"ti-search","--purple"],
    [/(web|fetch|http|url|browse)/,"ti-world","--teal"],
    [/(task|agent|spawn|delegate)/,"ti-robot","--accent2"],
    [/(git|commit|diff|branch|pr|pull)/,"ti-git-branch","--accent2"],
    [/(todo|plan|list)/,"ti-list-check","--muted"],
  ];
  for(const [re,ic,col] of rules) if(re.test(n)) return {ic,col};
  return {ic:"ti-tool",col:"--muted"};
}
// Light inline formatting for assistant text: `code` and **bold**.
function wlFmt(t){
  return esc(t)
    .replace(/`([^`]+)`/g,'<code>$1</code>')
    .replace(/\*\*([^*]+)\*\*/g,'<b>$1</b>');
}
// Parse the raw live log (a common line protocol both engines emit) into items.
//   💬 <text>        assistant message (may span following unmarked lines)
//   🔧 name(args)    tool call
//   ↳ result (N…)    tool result
//   # <header>       run-start / meta divider
//   — run finished — end marker
// Unmarked non-empty lines (e.g. opencode's plain streamed text) attach to the
// current message, so engines that emit no emojis still render as messages.
function parseWorklog(raw){
  const items=[]; let cur=null;
  const flush=()=>{ if(cur){ cur.text=cur.text.replace(/\s+$/,""); if(cur.text) items.push(cur); cur=null; } };
  for(const line of (raw||"").split("\n")){
    const s=line.replace(/\s+$/,"");
    if(/^#\s/.test(s)){ flush(); items.push({k:"meta",text:s.replace(/^#\s*/,"")}); continue; }
    if(/^\s*—\s*run finished/.test(s)){ flush(); items.push({k:"end"}); continue; }
    if(s.startsWith("💬")){ flush(); cur={k:"msg",text:s.slice(2).replace(/^\s+/,"")}; continue; }
    if(s.startsWith("🔧")){ flush(); const m=s.slice(2).trim(); const i=m.indexOf("(");
      items.push({k:"tool",name:i>=0?m.slice(0,i):m,args:i>=0?m.slice(i+1).replace(/\)$/,""):""}); continue; }
    // Result line: the backend now emits a human summary ("↳ ✓ 220 passed");
    // the older "↳ result (396 chars)" form is still parsed for old logs.
    const rm=s.match(/^\s*↳\s*(.+?)\s*$/);
    if(rm){ flush(); const old=rm[1].match(/^result\s*\(([^)]*)\)$/); items.push({k:"result",info:old?old[1]:rm[1],body:[]}); continue; }
    // Preview line (actual output, indented with ┆): attach to the last result.
    const pv=s.match(/^\s*┆ ?(.*)$/);
    if(pv){ const last=items[items.length-1]; if(last&&last.k==="result"){ (last.body=last.body||[]).push(pv[1]); } continue; }
    // continuation / plain text
    if(cur&&cur.k==="msg") cur.text+="\n"+s;
    else if(s.trim()) cur={k:"msg",text:s};
  }
  flush();
  return items;
}
// A review/structured-output line is often the model's final answer dumped as
// raw JSON — `{"decision":"approve","summary":"…"}`. Rendered verbatim it is a
// wall of braces; parse it into a verdict badge + the summary as prose.
function wlVerdictCard(text){
  const t=String(text||"").trim();
  if(!t.startsWith("{")||!/"(decision|verdict)"/.test(t))return null;
  let o;try{o=JSON.parse(t);}catch(e){return null;}
  const d=String(o.decision||o.verdict||"").toLowerCase();
  if(!d)return null;
  const summary=o.summary||o.reason||o.rationale||"";
  const ok=/approve|pass|verified|accept|merge/.test(d);
  const bad=/reject|request_changes|request-changes|fail|block|deny/.test(d);
  const col=ok?"--green":(bad?"--red":"--amber");
  const label=d.replace(/_/g," ").replace(/\b\w/g,c=>c.toUpperCase());
  return `<div class="wl-item wl-msg"><span class="wl-ic"><i class="ti ti-gavel"></i></span>
    <div class="wl-body"><span class="wl-verdict" style="background:color-mix(in srgb,var(${col}) 15%,transparent);color:var(${col})">${esc(label)}</span>
    ${summary?`<div class="wl-verdict-sum">${wlFmt(String(summary))}</div>`:""}</div></div>`;
}
function renderWorklog(items,live){
  const parts=items.map(it=>{
    if(it.k==="meta") return `<div class="wl-item wl-meta"><i class="ti ti-player-play"></i> ${esc(it.text)}</div>`;
    if(it.k==="end")  return `<div class="wl-item wl-end"><span><i class="ti ti-circle-check"></i> run finished</span></div>`;
    if(it.k==="tool"){ const m=wlToolMeta(it.name);
      // Harness-control calls read like errors to a person ("ScheduleWakeup
      // {stop:true}"?!) — annotate them in plain language instead.
      const note=wlHarnessNote(it.name,it.args);
      if(note) return `<div class="wl-item wl-tool"><span class="wl-chip"><i class="ti ti-clock-pause wl-tic" style="color:var(--muted)"></i><span class="wl-tname">${esc(note)}</span></span></div>`;
      const said=wlSay(it.name,it.args);
      return `<div class="wl-item wl-tool"><span class="wl-chip"><i class="ti ${m.ic} wl-tic" style="color:var(${m.col})"></i><span class="wl-tname">${esc(said.verb)}</span>${said.detail?`<span class="wl-targs">${esc(said.detail)}</span>`:""}</span></div>`; }
    if(it.k==="result"){
      const body=(it.body||[]).filter(x=>x!=null);
      const head=`<i class="ti ti-corner-down-right"></i> ${esc(it.info)}`;
      if(!body.length) return `<div class="wl-item wl-result"><span class="wl-rin">${head}</span></div>`;
      // The real output, revealed on click — a summary you can open, not a
      // dead-end count.
      return `<div class="wl-item wl-result"><details class="wl-out"><summary class="wl-rin">${head} <span class="wl-more">show output</span></summary><pre class="wl-pre">${esc(body.join("\n"))}</pre></details></div>`;
    }
    const card=wlVerdictCard(it.text);
    if(card) return card;
    return `<div class="wl-item wl-msg"><span class="wl-ic"><i class="ti ti-sparkles"></i></span><div class="wl-body">${wlFmt(it.text)}</div></div>`;
  });
  if(live) parts.push(`<div class="wl-typing"><span class="wl-ic"><i class="ti ti-sparkles"></i></span><span class="dots"><i></i><i></i><i></i></span></div>`);
  return parts.join("");
}
async function pollAgentLog(){
  if(!AGENT_LOG_ROLE)return;
  const body=document.getElementById("agent-transcript");
  const badge=document.getElementById("agent-live-badge");
  try{
    const url="/agent-log?role="+encodeURIComponent(AGENT_LOG_ROLE)+(AGENT_LOG_WORKER?"&worker="+encodeURIComponent(AGENT_LOG_WORKER):"");
    const d=await(await fetch(api(url))).json();
    if(badge)badge.style.display=d.live?"inline-block":"none";
    const raw=(d.log||"").trim();
    // Skip the DOM churn (and preserve the user's scroll) when nothing changed.
    const sig=raw+"|"+(d.live?1:0);
    if(sig===AGENT_LOG_LAST)return;
    AGENT_LOG_LAST=sig;
    const atBottom=body.scrollHeight-body.scrollTop-body.clientHeight<60;
    if(!raw){
      body.innerHTML='<div class="wl-empty"><i class="ti ti-moon-stars"></i> this agent hasn\'t run yet</div>';
      return;
    }
    body.innerHTML=renderWorklog(parseWorklog(raw),d.live);
    if(atBottom)body.scrollTop=body.scrollHeight;   // follow the tail
  }catch(e){}
}
function closeAgent(){clearInterval(AGENT_LOG_TIMER);AGENT_LOG_TIMER=null;AGENT_LOG_ROLE=null;AGENT_LOG_WORKER="";close_('ov-agent');}
async function openTranscript(enc,name){
  document.getElementById("tr-title").textContent=name;
  document.getElementById("tr-body").textContent="loading…";
  document.getElementById("ov-transcript").classList.add("open");
  try{const r=await fetch(api("/transcripts/"+enc));document.getElementById("tr-body").textContent=r.ok?await r.text():"(unavailable)";}
  catch(e){document.getElementById("tr-body").textContent="(error)";}
}
function renderTwofa(){
  const el=document.getElementById("twofa-body");if(!el)return;
  const on=ME&&ME.twofa;
  if(on){
    el.innerHTML=`<div style="display:flex;align-items:center;gap:10px">
      <span class="tk" style="background:color-mix(in srgb,var(--green) 13%,transparent);color:var(--green)"><i class="ti ti-shield-check" style="font-size:13px"></i> enabled</span>
      <span style="font-size:13px;color:var(--muted)">2FA is active for <b>${esc(ME.username)}</b>.</span>
      <div style="flex:1"></div>
      <button onclick="disable2fa()" style="background:transparent;color:var(--red);border:1px solid var(--red);border-radius:8px;padding:7px 13px;font-size:12px;font-weight:600;cursor:pointer;font-family:inherit">Disable</button></div>`;
    return;
  }
  el.innerHTML=`<div style="font-size:13px;color:var(--muted);margin-bottom:10px">Protect <b>${esc(ME?ME.username:'')}</b> with an authenticator app (Google Authenticator, 1Password…).</div>
    <button class="pri" onclick="enroll2fa()"><i class="ti ti-shield-plus"></i> Set up 2FA</button>
    <div id="twofa-enroll" style="margin-top:12px"></div>`;
}
async function enroll2fa(){
  try{
    const r=await fetch("/api/auth/2fa/enroll",{method:"POST"});
    if(!r.ok)return;
    const {secret,uri}=await r.json();
    document.getElementById("twofa-enroll").innerHTML=`
      <div class="panel" style="border-color:var(--accent2)">
        <div style="font-size:12px;color:var(--muted);margin-bottom:8px">1. Add this secret to your authenticator app:</div>
        <code style="display:block;word-break:break-all;font-size:12px;background:var(--card2);padding:9px 11px;border-radius:8px;margin-bottom:6px">${esc(secret)}</code>
        <div style="font-size:11px;color:var(--dim);word-break:break-all;margin-bottom:10px">${esc(uri)}</div>
        <div style="font-size:12px;color:var(--muted);margin-bottom:6px">2. Enter the current 6-digit code to confirm:</div>
        <div style="display:flex;gap:8px">
          <input id="twofa-code" inputmode="numeric" maxlength="6" placeholder="000000" style="flex:1;background:var(--card2);color:var(--text);border:1px solid var(--border2);border-radius:8px;padding:9px 11px;font-size:13px;font-family:inherit;letter-spacing:3px" onkeydown="if(event.key==='Enter')confirm2fa()">
          <button class="pri" onclick="confirm2fa()">Confirm</button></div>
        <div id="twofa-err" style="color:var(--red);font-size:12px;min-height:14px;margin-top:6px"></div></div>`;
    setTimeout(()=>document.getElementById("twofa-code").focus(),50);
  }catch(e){}
}
async function confirm2fa(){
  const code=document.getElementById("twofa-code").value.trim();
  const err=document.getElementById("twofa-err");
  if(!code){err.textContent="Enter the code.";return;}
  try{
    const r=await fetch("/api/auth/2fa/enable",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({code})});
    if(!r.ok){err.textContent=await r.text()||"Invalid code.";return;}
    ME.twofa=true;renderTwofa();
  }catch(e){err.textContent="Network error.";}
}
async function disable2fa(){try{await fetch("/api/auth/2fa/disable",{method:"POST"});ME.twofa=false;renderTwofa();}catch(e){}}
function renderAccess(){
  renderTwofa();
  renderUsers();
  const list=document.getElementById("tok-list");
  fetch("/api/auth/tokens").then(r=>r.ok?r.json():[]).then(rows=>{
    if(!Array.isArray(rows)||!rows.length){list.innerHTML='<div class="empty">no API tokens yet</div>';return;}
    list.innerHTML=rows.map(t=>{const admin=t.role==="admin";
      return `<div class="arow"><div class="aav" style="background:linear-gradient(135deg,var(--accent2),var(--accent))"><i class="ti ti-key" style="font-size:14px"></i></div>
        <div style="flex:1;min-width:0"><div class="aname">${esc(t.label)}</div><div class="ameta">created ${esc((t.created||"").slice(0,16).replace("T"," "))}</div></div>
        <span class="arole ${admin?'admin':'viewer'}">${esc(t.role)}</span>
        <button class="adel" onclick="revokeToken('${encodeURIComponent(t.label)}')" title="Revoke"><i class="ti ti-trash"></i></button></div>`;}).join("");
  }).catch(()=>{list.innerHTML='<div class="empty">unable to load tokens</div>';});
}
async function mintToken(){
  const label=document.getElementById("tok-label").value.trim();
  const role=document.getElementById("tok-role").value;
  const box=document.getElementById("tok-secret");
  if(!label){box.innerHTML='<div style="color:var(--red);font-size:12px;margin-bottom:10px">Enter a label.</div>';return;}
  try{
    const r=await fetch("/api/auth/tokens",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({label,role})});
    if(!r.ok){box.innerHTML=`<div style="color:var(--red);font-size:12px;margin-bottom:10px">${esc(await r.text())||"Failed."}</div>`;return;}
    const {token}=await r.json();
    document.getElementById("tok-label").value="";
    box.innerHTML=`<div class="panel" style="border-color:var(--accent2);margin-bottom:12px">
      <div style="font-size:12px;color:var(--muted);margin-bottom:6px"><i class="ti ti-alert-triangle" style="color:var(--amber)"></i> Copy this now — it is not shown again:</div>
      <code style="display:block;word-break:break-all;font-size:12px;background:var(--card2);padding:9px 11px;border-radius:8px">${esc(token)}</code></div>`;
    renderAccess();
  }catch(e){box.innerHTML='<div style="color:var(--red);font-size:12px">Network error.</div>';}
}
async function revokeToken(label){try{await fetch("/api/auth/tokens/"+label,{method:"DELETE"});renderAccess();}catch(e){}}
function renderAudit(){
  fetch("/api/audit-log").then(r=>r.ok?r.json():[]).then(rows=>{
    const el=document.getElementById("audit-body");
    if(!Array.isArray(rows)||!rows.length){el.innerHTML='<div class="empty">no audited actions yet</div>';return;}
    el.innerHTML=rows.map(e=>{
      const bad=e.status>=400;const col=bad?"var(--red)":"var(--green)";
      const ic=e.action.includes("login")?(bad?"lock-x":"login"):"pencil";
      return `<div class="act"><div class="ad" style="background:${col}22;color:${col}"><i class="ti ti-${ic}" style="font-size:13px"></i></div>
        <div class="atx"><span class="who">${esc(e.user)}</span> ${esc(e.action)} <span class="tk" style="background:${col}22;color:${col}">${e.status}</span></div>
        <span class="tm">${esc((e.at||"").slice(5,16).replace("T"," "))}</span></div>`;}).join("");
  }).catch(()=>{document.getElementById("audit-body").innerHTML='<div class="empty">unable to load audit log</div>';});
}
function relTime(iso){if(!iso)return "never";const t=Date.parse(iso.replace(" ","T"));if(isNaN(t))return esc(iso.slice(5,16));
  const s=(Date.now()-t)/1000;if(s<90)return "just now";if(s<3600)return Math.round(s/60)+"m ago";if(s<86400)return Math.round(s/3600)+"h ago";return Math.round(s/86400)+"d ago";}
function isActiveRecent(iso){if(!iso)return false;const t=Date.parse(iso.replace(" ","T"));return !isNaN(t)&&(Date.now()-t)<86400000;}
function renderPeople(){
  const body=document.getElementById("people-body");
  body.innerHTML='<div class="empty">loading…</div>';
  fetch("/api/people-analytics").then(r=>r.ok?r.json():null).then(d=>{
    if(!d){body.innerHTML='<div class="empty">admin role required</div>';return;}
    const people=d.people||[];
    document.getElementById("people-sample").textContent=(d.sample||0)+" events analysed";
    const active=people.filter(p=>isActiveRecent(p.last_active)).length;
    const working=people.filter(p=>p.work>0).length;
    const totalWork=people.reduce((a,p)=>a+(p.work||0),0);
    document.getElementById("people-kpis").innerHTML=
      kpi("Active 24h",active)+kpi("Contributors",working)+kpi("People",people.length)+kpi("Work actions",totalWork);
    if(!people.length){body.innerHTML='<div class="empty">no activity recorded yet</div>';return;}
    const maxW=Math.max(1,...people.map(p=>p.work||0));
    const chart=`<div class="sec" style="margin-bottom:8px">Productivity <span style="font-size:11px;color:var(--dim);font-weight:400">· work actions per person + success rate</span></div>
      <div class="panel pchart">${people.map(p=>{const w=Math.round((p.work||0)/maxW*100);
        const sc=p.success_rate>=95?"var(--green)":(p.success_rate>=80?"var(--amber)":"var(--red)");
        return `<div class="pcrow">
          <div class="pclabel"><span class="pav" style="width:22px;height:22px;font-size:10px">${esc((p.user||'?').charAt(0).toUpperCase())}</span> ${esc(p.user)}</div>
          <div class="pcbarwrap"><div class="pcbar" style="width:${Math.max(2,w)}%"></div><span class="pcval">${p.work} work · ${p.actions} total</span></div>
          <div class="pcsuccess" title="success rate ${p.success_rate}%"><div class="pcsuccessbar" style="width:${p.success_rate}%;background:${sc}"></div></div>
          <div style="font-size:11px;color:${sc};min-width:40px;text-align:right">${p.success_rate}%</div></div>`;}).join("")}</div>
      <div class="sec" style="margin-top:18px">Per-person detail</div>`;
    body.innerHTML=chart+'<table class="ptbl"><thead><tr><th>User</th><th>Status</th><th>Work</th><th>Total</th><th>Active days</th><th>Success</th><th>Last active</th><th>Top actions</th></tr></thead><tbody>'+
      people.map(p=>{
        const live=isActiveRecent(p.last_active);
        const working=p.work>0;
        const badge=live?'<span class="pbadge on">● active</span>':(working?'<span class="pbadge idle">idle</span>':'<span class="pbadge off">observer</span>');
        const sc=p.success_rate>=95?"var(--green)":(p.success_rate>=80?"var(--amber)":"var(--red)");
        const top=(p.top_actions||[]).map(a=>`<span class="tk">${esc(a.action)} ·${a.count}</span>`).join(" ")||'<span style="color:var(--muted)">—</span>';
        const initial=(p.user||"?").charAt(0).toUpperCase();
        return `<tr>
          <td><div class="pcell"><span class="pav">${esc(initial)}</span><b>${esc(p.user)}</b></div></td>
          <td>${badge}</td>
          <td><b style="color:${working?'var(--text)':'var(--muted)'}">${p.work}</b></td>
          <td>${p.actions}</td>
          <td>${p.active_days}</td>
          <td style="color:${sc}">${p.success_rate}%</td>
          <td style="color:var(--muted)">${relTime(p.last_active)}</td>
          <td>${top}</td></tr>`;
      }).join("")+'</tbody></table>';
  }).catch(()=>{body.innerHTML='<div class="empty">unable to load analytics</div>';});
}
/* ---- Toasts ---- */
function toast(msg,type){const c=document.getElementById("toasts");if(!c)return;
  const ic=type==="ok"?"circle-check":(type==="err"?"alert-triangle":"info-circle");
  const el=document.createElement("div");el.className="toast "+(type||"");el.innerHTML=`<i class="ti ti-${ic}"></i><span>${esc(msg)}</span>`;
  c.appendChild(el);setTimeout(()=>{el.classList.add("hide");setTimeout(()=>el.remove(),220);},3000);}
/* ---- Command palette (⌘K) ---- */
const NAV_IC={overview:"layout-dashboard",team:"robot",board:"columns",roadmap:"timeline",activity:"activity",discuss:"messages",insights:"coin",people:"user-star",audit:"shield-lock",access:"user-cog",settings:"settings"};
let CMDK_ITEMS=[],CMDK_SEL=0;
function cmdkBuild(){
  const items=[];const isAdmin=ME&&ME.role==="admin";
  Object.keys(TITLES).forEach(v=>{const a=document.querySelector(`.nav a[data-v="${v}"]`);if(!a)return;
    if(a.classList.contains("admin-only")&&!isAdmin)return;
    if(a.classList.contains("manage-only")&&!canManage())return;
    items.push({cat:"Go to",label:TITLES[v][0],hint:TITLES[v][1],icon:NAV_IC[v]||"point",run:()=>nav(v)});});
  (PROJECTS||[]).forEach(p=>items.push({cat:"Switch project",label:p.name,hint:p.alias||p.id,icon:"hexagon-letter-c",run:()=>switchProject(p.id)}));
  items.push({cat:"Action",label:"New ticket",hint:"add to backlog",icon:"plus",run:()=>openNewTicket()});
  if(isAdmin)items.push({cat:"Action",label:"New project",hint:"scaffold / import",icon:"folder-plus",run:()=>openNewProject()});
  items.push({cat:"Action",label:"Start / Resume the loop",icon:"player-play",run:()=>ctl("resume")});
  items.push({cat:"Action",label:"Pause the loop",icon:"player-pause",run:()=>ctl("pause")});
  items.push({cat:"Action",label:"Run one step",icon:"player-track-next",run:()=>ctl("step")});
  items.push({cat:"Action",label:"Sign out",icon:"logout",run:()=>doLogout()});
  return items;}
function openCmdk(){CMDK_ITEMS=cmdkBuild();CMDK_SEL=0;
  document.getElementById("cmdk").classList.add("open");const i=document.getElementById("cmdk-input");i.value="";cmdkFilter();setTimeout(()=>i.focus(),30);}
function closeCmdk(){document.getElementById("cmdk").classList.remove("open");}
function cmdkFilter(){
  const q=(document.getElementById("cmdk-input").value||"").toLowerCase().trim();
  const matches=CMDK_ITEMS.filter(it=>!q||it.label.toLowerCase().includes(q)||(it.hint||"").toLowerCase().includes(q)||it.cat.toLowerCase().includes(q));
  CMDK_SEL=Math.min(CMDK_SEL,Math.max(0,matches.length-1));
  const el=document.getElementById("cmdk-list");
  if(!matches.length){el.innerHTML='<div class="cmdk-empty">No matches</div>';window._cmdkMatches=[];return;}
  window._cmdkMatches=matches;let html="",lastCat="";
  matches.forEach((it,i)=>{if(it.cat!==lastCat){lastCat=it.cat;html+=`<div class="cmdk-cat">${esc(it.cat)}</div>`;}
    html+=`<div class="cmdk-item ${i===CMDK_SEL?'sel':''}" onmousemove="cmdkSel(${i})" onclick="cmdkRun(${i})">
      <span class="ci-ic"><i class="ti ti-${it.icon}"></i></span><span class="ci-t">${esc(it.label)}</span>${it.hint?`<span class="ci-h">${esc(it.hint)}</span>`:''}</div>`;});
  el.innerHTML=html;}
function cmdkSel(i){CMDK_SEL=i;document.querySelectorAll("#cmdk-list .cmdk-item").forEach((x,j)=>x.classList.toggle("sel",j===i));}
function cmdkRun(i){const m=window._cmdkMatches||[];if(m[i]){closeCmdk();try{m[i].run();}catch(e){}}}
function cmdkKey(e){const m=window._cmdkMatches||[];
  if(e.key==="Escape")closeCmdk();
  else if(e.key==="ArrowDown"){e.preventDefault();cmdkSel(Math.min(CMDK_SEL+1,m.length-1));scrollSel();}
  else if(e.key==="ArrowUp"){e.preventDefault();cmdkSel(Math.max(CMDK_SEL-1,0));scrollSel();}
  else if(e.key==="Enter"){e.preventDefault();cmdkRun(CMDK_SEL);}}
function scrollSel(){const s=document.querySelector("#cmdk-list .cmdk-item.sel");if(s)s.scrollIntoView({block:"nearest"});}
document.addEventListener("keydown",e=>{if((e.metaKey||e.ctrlKey)&&e.key.toLowerCase()==="k"){e.preventDefault();
  document.getElementById("cmdk").classList.contains("open")?closeCmdk():openCmdk();}});
// ── Chat enhancements: threads, edit, delete, pin, search, typing ────────
let THREAD_MSG=null, THREAD_LOADING=false;
const TYPING=new Map();
let TYPING_SENT=0;

function openThread(mid){
  if(!mid||mid.length<3){console.warn("openThread: invalid mid",mid);return;}
  THREAD_MSG=mid;THREAD_LOADING=true;
  document.getElementById("thread-side").classList.add("on");
  document.getElementById("thread-back").classList.add("on");
  document.getElementById("thread-title").textContent="Thread";
  document.getElementById("thread-msgs").innerHTML='<div class="empty" style="padding:20px">Loading…</div>';
  fetch("/api/chat/messages/"+mid+"/thread").then(r=>r.json()).then(d=>{
    THREAD_LOADING=false;
    const box=document.getElementById("thread-msgs");
    const parent=d.parent?renderOneMsg(d.parent,"thread"):"";
    const replies=(d.replies||[]).map(m=>renderOneMsg(m,"thread")).join("");
    box.innerHTML=parent+'<div style="padding:8px 0;font-size:12px;color:var(--dim);font-weight:600;border-bottom:1px solid var(--border);margin-bottom:8px">'+(d.replies||[]).length+' repl'+(d.replies.length!==1?'ies':'y')+'</div>'+replies;
    box.scrollTop=box.scrollHeight;
  }).catch(e=>{document.getElementById("thread-msgs").innerHTML='<div class="empty">Failed to load</div>';});
}
function closeThread(){document.getElementById("thread-side").classList.remove("on");document.getElementById("thread-back").classList.remove("on");THREAD_MSG=null;}
// Escape closes chat overlays and the thread panel (they only had backdrop/X
// close before). Skip when coxModal is up — it has its own Escape and sits on top.
document.addEventListener("keydown",e=>{
  if(e.key!=="Escape")return;
  const cm=document.getElementById("cox-modal");if(cm&&!cm.hidden)return;
  const th=document.getElementById("thread-side");
  if(th&&th.classList.contains("on")){closeThread();return;}
  for(const id of ["ov-chset","ov-members","ov-webhooks"]){
    const el=document.getElementById(id);
    if(el&&el.classList.contains("open")){close_(id);return;}
  }
});
async function sendThreadReply(){
  if(!THREAD_MSG){console.warn("sendThreadReply: no THREAD_MSG");return;}
  const inp=document.getElementById("thread-input");
  if(!inp){console.warn("sendThreadReply: no input");return;}
  const body=inp.value.trim();if(!body){console.warn("sendThreadReply: empty body");return;}
  try{
    const r=await fetch("/api/chat/messages/"+THREAD_MSG+"/reply",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({body})});
    if(r.ok){inp.value="";openThread(THREAD_MSG);}
    else console.warn("sendThreadReply: server error", r.status, await r.text());
  }catch(e){console.warn("sendThreadReply: network error", e);}
}
function renderOneMsg(m,ctx){
  if(ctx==="chat")return renderSlackMsg(m);
  const time=m.at?new Date(m.at).toLocaleTimeString([],{hour:"2-digit",minute:"2-digit"}):"";
  const isMe=m.user===ME?.username;
  const deleted=m.deleted;
  const bodyH=deleted?'<div class="tcbub deleted-msg"><i>This message was deleted</i></div>'
    :`<div class="tcbub" title="${esc(m.user)} · ${time}">${formatMsg(m.body)}${m.edited?` <span class="tcedit">(edited)</span>`:''}</div>`;
  const atts=m.attachments&&m.attachments.length?m.attachments.map(a=>
    a.mime&&a.mime.startsWith("image/")?`<img src="${esc(a.url)}" class="tci" loading="lazy" onclick="window.open('${esc(a.url)}','_blank')">`
    :`<a href="${esc(a.url)}" class="tcfile" download>${esc(a.name)}</a>`).join(""):"";
  const reacts=m.reactions&&m.reactions.length?'<div class="tcr">'+m.reactions.map(r=>`<span class="tcre${r.users.includes(ME?.username)?' me':''}" onclick="reactTo('${esc(m.id)}','${esc(r.emoji)}')" title="${r.users.map(memberName).join(', ')}">${esc(r.emoji)} ${r.users.length}</span>`).join("")+`<span class="tcre add" onclick="openEmoji(document.getElementById('tcr-${esc(m.id)}'),e=>reactTo('${esc(m.id)}',e))" id="tcr-${esc(m.id)}"><i class="ti ti-mood-plus"></i></span></div>`:'';
  const hasId=m.id&&m.id.length>5;
  const acts=deleted||!hasId?"":`<div class="tcacts"><button onclick="openThread('${esc(m.id)}')" title="Reply in thread"><i class="ti ti-message-2"></i> ${m.reply_count||''}</button><button onclick="startEditMsg('${esc(m.id)}')" title="Edit"><i class="ti ti-pencil"></i></button><button onclick="deleteMsg('${esc(m.id)}')" title="Delete"><i class="ti ti-trash"></i></button><button onclick="togglePin('${esc(m.id)}')" title="Pin"><i class="ti ti-pinned"></i></button><button onclick="copyMsgLink('${esc(m.id)}')" title="Copy link"><i class="ti ti-link"></i></button></div>`;
  return `<div class="tcmsg${isMe?' me':''}${deleted?' deleted':''}" id="msg-${esc(m.id)}">
    ${ctx!=="thread"&&!m.grouped?`<div class="tchdr"><span class="tcu">${esc(memberName(m.user))}</span><span class="tct">${time}</span></div>`:''}
    ${bodyH}${atts}${reacts}${acts}</div>`;
}
// Slack-style flat row: avatar gutter + bold name/time header, hover-reveal
// action toolbar, full-width body (no bubbles, no right-aligned "me").
function renderSlackMsg(m){
  const time=m.at?new Date(m.at).toLocaleTimeString([],{hour:"2-digit",minute:"2-digit"}):"";
  const me=ME?.username,isMe=m.user===me,deleted=m.deleted,hasId=m.id&&m.id.length>5;
  const col=userColor(m.user);
  const body=deleted?'<div class="smsg-body deleted"><i>This message was deleted</i></div>'
    :(m.body?`<div class="smsg-body">${formatMsg(m.body)}${m.edited?' <span class="tcedit">(edited)</span>':''}</div>`:'');
  const reacts=(!deleted&&m.reactions&&m.reactions.length)?'<div class="tcr">'+m.reactions.map(r=>`<span class="tcre${r.users.includes(me)?' me':''}" onclick="reactTo('${esc(m.id)}','${esc(r.emoji)}')" title="${r.users.map(memberName).join(', ')}">${esc(r.emoji)} ${r.users.length}</span>`).join("")+`<span class="tcre add" onclick="openEmoji(document.getElementById('tcr-${esc(m.id)}'),e=>reactTo('${esc(m.id)}',e))" id="tcr-${esc(m.id)}"><i class="ti ti-mood-plus"></i></span></div>`:'';
  // Slack's signature: a clickable "N replies" bar under threaded parents.
  const thread=(!deleted&&m.reply_count>0)?`<div class="sthread" onclick="openThread('${esc(m.id)}')"><i class="ti ti-message-2"></i> ${m.reply_count} ${m.reply_count>1?'replies':'reply'} <span class="sthread-go">View thread ›</span></div>`:'';
  const own=isMe&&!deleted&&hasId;
  const acts=(!deleted&&hasId)?`<div class="sacts">
      <button onclick="openEmoji(this,e=>reactTo('${esc(m.id)}',e))" title="React"><i class="ti ti-mood-plus"></i></button>
      <button onclick="openThread('${esc(m.id)}')" title="Reply in thread"><i class="ti ti-message-2"></i></button>
      ${own?`<button onclick="startEditMsg('${esc(m.id)}')" title="Edit"><i class="ti ti-pencil"></i></button>
      <button onclick="deleteMsg('${esc(m.id)}')" title="Delete"><i class="ti ti-trash"></i></button>`:''}
      <button onclick="togglePin('${esc(m.id)}')" title="Pin"><i class="ti ti-pinned"></i></button>
      <button onclick="copyMsgLink('${esc(m.id)}')" title="Copy link"><i class="ti ti-link"></i></button>
    </div>`:'';
  const gutter=m.grouped?`<span class="sgt">${time}</span>`:avat(m.user,"sav");
  // Hybrid gate announcements become ACTION CARDS: the decision is one click
  // away from the message that asked for it (see docs/HYBRID_TEAM.md).
  const gate=(!deleted&&m.user==="SYSTEM")?gateActions(m.body):"";
  return `<div class="smsg${m.grouped?' grouped':''}" id="msg-${esc(m.id)}">
    <div class="sgut">${gutter}</div>
    <div class="smain">
      ${m.grouped?'':`<div class="shdr"><span class="snm" style="color:${col}">${esc(memberName(m.user))}</span>${statusChip(m.user)}<span class="stm">${time}</span></div>`}
      ${body}${attHtml(m.attachments)}${reacts}${gate}${thread}
    </div>${acts}</div>`;
}
// Inline approve/verify buttons for gate-hold SYSTEM messages. The ticket id
// is parsed from the message; the buttons call the same endpoints as the
// Inbox, so chat and Inbox stay two doors to one decision.
function gateActions(body){
  const b=String(body||"");
  const id=(b.match(/\b([A-Z][A-Z0-9]+-[BF]\d+)\b/)||[])[1];
  if(!id)return "";
  const btn=(label,ic,fn,pri)=>`<button class="tk-btn${pri?' go':''}" style="padding:4px 12px;font-size:12px" onclick="${fn}"><i class="ti ${ic}"></i> ${label}</button>`;
  if(b.includes("gate_ready"))
    return `<div style="display:flex;gap:8px;margin-top:7px">${btn("Approve → Ready","ti-checks",`chatGate('${id}','ready')`,1)}${btn("Open ticket","ti-external-link",`showTicket('${id}')`)}</div>`;
  if(b.includes("awaiting HUMAN verification")||b.includes("gate_verify"))
    return `<div style="display:flex;gap:8px;margin-top:7px">${btn("Mark Verified","ti-shield-check",`chatGate('${id}','verify')`,1)}${btn("Open ticket","ti-external-link",`showTicket('${id}')`)}</div>`;
  return "";
}
async function chatGate(id,action){
  try{const r=await fetch(api("/ticket/"+encodeURIComponent(id)+"/"+action),{method:"POST",headers:{"Content-Type":"application/json"},body:"{}"});
    if(!r.ok)toasty(await r.text(),"err");
    else toasty(action==="ready"?(id+" → Ready"):(id+" verified"),"ok");
  }catch(e){}
}
// Wrap the selection of any input/textarea in a markdown marker (**,*,`).
function wrapField(el,mk){if(!el)return;
  const a=el.selectionStart||0,b=el.selectionEnd||0,v=el.value;const sel=v.slice(a,b)||"text";
  el.value=v.slice(0,a)+mk+sel+mk+v.slice(b);el.focus();
  el.setSelectionRange(a+mk.length,a+mk.length+sel.length);}
// Insert text (e.g. a picked emoji) at the caret of any input/textarea.
function insertAtCaret(el,text){if(!el)return;
  const a=el.selectionStart||el.value.length,b=el.selectionEnd||a,v=el.value;
  el.value=v.slice(0,a)+text+v.slice(b);el.focus();
  el.setSelectionRange(a+text.length,a+text.length);}
// The same B / I / code / emoji controls as the main composer, for any field.
function minibarHtml(fieldId){return `<div class="minibar">
    <button title="Bold" onclick="wrapField(document.getElementById('${fieldId}'),'**')"><i class="ti ti-bold"></i></button>
    <button title="Italic" onclick="wrapField(document.getElementById('${fieldId}'),'*')"><i class="ti ti-italic"></i></button>
    <button title="Code" onclick="wrapField(document.getElementById('${fieldId}'),'\\u0060')"><i class="ti ti-code"></i></button>
    <button title="Emoji" onclick="openEmoji(this,e=>insertAtCaret(document.getElementById('${fieldId}'),e))"><i class="ti ti-mood-smile"></i></button>
  </div>`;}
function startEditMsg(id,body){
  const el=document.getElementById("msg-"+id);if(!el)return;
  const bub=el.querySelector(".tcbub")||el.querySelector(".smsg-body");if(!bub)return;
  const original=bub.innerHTML;
  const m=CHAT.find(x=>x.id===id);
  const text=decodeURIComponent(body||(m&&m.body)||bub.textContent||"");
  bub.innerHTML=`${minibarHtml("edit-"+id)}
    <textarea class="edit-inline" id="edit-${id}" rows="2">${esc(text)}</textarea>
    <div class="edit-hint">esc to cancel · enter to save ·
      <a style="cursor:pointer;color:var(--accent2)" onclick="saveEditInline('${id}',null)">save</a> ·
      <a style="cursor:pointer" onclick="renderChatList(true)">cancel</a></div>`;
  const ta=document.getElementById("edit-"+id);
  ta.focus();ta.setSelectionRange(ta.value.length,ta.value.length);
  ta.onkeydown=e=>{
    if(e.key==="Enter"&&!e.shiftKey){e.preventDefault();saveEditInline(id,original);}
    if(e.key==="Escape"){e.preventDefault();cancelEditInline(id,original);}
  };
}
function cancelEditInline(id,original){
  if(original==null){renderChatList(true);return;}
  const el=document.getElementById("msg-"+id);if(!el)return;
  const bub=el.querySelector(".tcbub")||el.querySelector(".smsg-body");
  if(bub)bub.innerHTML=original;
}
async function saveEditInline(id,original){
  const ta=document.getElementById("edit-"+id);if(!ta)return;
  const body=ta.value.trim();if(!body){cancelEditInline(id,original);return;}
  try{
    const r=await fetch("/api/chat/messages/"+id,{method:"PATCH",headers:{"Content-Type":"application/json"},body:JSON.stringify({body})});
    if(!r.ok){toasty("Could not edit","err");cancelEditInline(id,original);}
  }catch(e){toasty("Network error","err");cancelEditInline(id,original);}
}
function cancelEdit(id){renderChatList(true);}
async function deleteMsg(id){
  if(!await coxModal({title:"Delete message?",message:"This can't be undone.",confirmText:"Delete",danger:true}))return;
  try{const r=await fetch("/api/chat/messages/"+id,{method:"DELETE"});if(!r.ok)toasty("Cannot delete","err");}
  catch(e){toasty("Network error","err");}}
async function togglePin(id){
  try{const r=await fetch("/api/chat/messages/"+id+"/pin",{method:"POST"});const d=await r.json();
    toasty(d.pinned?"Pinned":"Unpinned","ok");loadPins();
  }catch(e){toasty("Network error","err");}}
function copyMsgLink(id){
  const url=location.origin+location.pathname+"#msg-"+id;
  navigator.clipboard.writeText(url).then(()=>toasty("Link copied!","ok")).catch(()=>{});}
async function loadPins(){
  const ch=getActiveChannel();if(!ch)return;
  try{const r=await fetch("/api/chat/pins?channel="+ch);const pins=await r.json();
    const el=document.getElementById("pins-bar");
    if(!pins.length){el.style.display="none";return;}
    el.style.display="flex";
    el.innerHTML=pins.map(m=>`<span class="pin-chip" onclick="document.getElementById('msg-${esc(m.id)}')?.scrollIntoView({behavior:'smooth',block:'center'});closeThread();" title="${esc(m.user)}: ${esc(m.body.substring(0,80))}">📌 ${esc(m.body.substring(0,60))}</span>`).join("")+
      `<span style="color:var(--dim);font-size:11px">${pins.length} pinned</span>`;
  }catch(e){}}
function startTyping(){
  const now=Date.now();if(now-TYPING_SENT<2000)return;TYPING_SENT=now;
  const ch=getActiveChannel();if(!ch||!CHATWS||CHATWS.readyState!==1)return;
  CHATWS.send(JSON.stringify({op:"typing",channel:ch}));}
let TYPING_TIMER=null;
function renderTyping(){
  const el=document.getElementById("typing-bar");if(!el)return;
  const typers=[...TYPING.entries()].filter(([u,t])=>Date.now()-t<5000&&u!==ME?.username).map(([u])=>memberName(u));
  if(!typers.length){el.style.display="none";clearTimeout(TYPING_TIMER);TYPING_TIMER=null;return;}
  el.style.display="block";el.innerHTML=typers.length===1?`<span class="typing-dots">${esc(typers[0])} is typing<span>.</span><span>.</span><span>.</span></span>`
    :`${esc(typers.slice(0,2).join(", "))}${typers.length>2?' and '+(typers.length-2)+' more':''} typing…`;
  // Re-check when the oldest entry crosses the 5s window, so the indicator
  // clears itself even if no further chat event triggers a render.
  clearTimeout(TYPING_TIMER);TYPING_TIMER=setTimeout(renderTyping,1200);}
let USER_STATUS="online";
function setUserStatus(s){USER_STATUS=s;localStorage.setItem("cox_status",s);
  if(CHATWS&&CHATWS.readyState===1)CHATWS.send(JSON.stringify({op:"status",status:s}));
  document.querySelectorAll(".status-dot").forEach(d=>{d.className="status-dot "+s;});}
function toggleSearch(){const bar=document.getElementById("chat-search-bar");bar.style.display=bar.style.display==="flex"?"none":"flex";if(bar.style.display==="flex")document.getElementById("chat-search-input").focus();}
function chatSearch(q){
  if(!q||q.length<2){document.getElementById("search-results").innerHTML="";return;}
  fetch("/api/chat/search?q="+encodeURIComponent(q)).then(r=>r.json()).then(msgs=>{
    const el=document.getElementById("search-results");
    el.innerHTML=msgs.length?msgs.map(m=>`<div class="search-hit" onclick="gotoMsg('${esc(m.id)}','${esc(m.channel||'')}');document.getElementById('chat-search-bar').style.display='none';">
        <span class="sht">${esc(memberName(m.user))}</span> <span class="shb">${esc(m.body.substring(0,120))}</span>
        <span class="shtime">${esc((m.at||'').slice(5,16).replace('T',' '))}</span></div>`).join(""):'<div class="empty" style="padding:6px 12px">No results</div>';});}

// ── Slash commands ───────────────────────────────────────────────────────
function handleSlashCommand(){
  const inp=document.getElementById("chat-input");if(!inp)return;
  const v=inp.value.trim();
  if(!v.startsWith("/"))return false;
    const [cmd,...args]=v.slice(1).split(/\s+/);
  const arg=args.join(" ");
  switch(cmd){
    case "me": case "shrug": case "tableflip":{
      const text=cmd==="shrug"?`¯\\_(ツ)_/¯`:cmd==="tableflip"?`(╯°□°)╯︵ ┻━┻`:arg?`_${arg}_`:"";
      inp.value=text;return true;
    }
    case "invite":{
      const ch=currentChannel();if(ch.kind!=="private"){toasty("Only private channels","err");break;}
      const u=arg.replace(/^@/,"");if(!u){toasty("Usage: /invite @username","err");break;}
      fetch(api("/chat/channels/"+ch.id+"/invite"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({user:u})})
        .then(r=>r.ok?r.json():r.text().then(t=>Promise.reject(t)))
        .then(()=>{toasty(u+" invited","ok");loadChannels();}).catch(e=>toasty(e,"err"));
      break;}
    case "topic": case "description":{
      const ch=currentChannel();const topic=arg;
      fetch("/api/chat/channel/"+ch.id+"/topic",{method:"PATCH",headers:{"Content-Type":"application/json"},body:JSON.stringify({topic})})
        .then(r=>r.json()).then(d=>{toasty("Topic updated","ok");updateChatHeader();}).catch(()=>{});
      break;}
    case "mute":
      if(!isMuted(CURCHAN)){MUTED.add(CURCHAN);saveMuted();toasty("Channel muted","ok");updateChannelBell();}
      break;
    case "unmute":
      if(isMuted(CURCHAN)){MUTED.delete(CURCHAN);saveMuted();toasty("Channel unmuted","ok");updateChannelBell();}
      break;
    case "away": case "back": case "online": case "busy": case "dnd":
      setUserStatus(cmd==="dnd"?"busy":cmd==="back"?"online":cmd);
      toasty("Status: "+USER_STATUS,"ok");
      break;
    default:
      toasty("Unknown command. Try /invite, /topic, /mute, /away, /busy, /online","err");
  }
  inp.value="";return true;
}

// ── Read receipts ────────────────────────────────────────────────────────
let READ_RECEIPTS={};
function markRead(channel){
  if(!channel||!ME?.username)return;
  const msgs=CHAT.filter(m=>m.channel===channel&&m.user!==ME.username);
  if(!msgs.length)return;
  const last=msgs[msgs.length-1];
  if(!READ_RECEIPTS[channel]||READ_RECEIPTS[channel].id!==last.id){
    READ_RECEIPTS[channel]={id:last.id, at:last.at, count:msgs.length};
    if(CHATWS&&CHATWS.readyState===1)CHATWS.send(JSON.stringify({op:"read",channel}));
  }
}
function seenByText(read){if(!read)return"";
  return ` <span class="seen-by" title="Seen">✓</span>`;}

// ── User status UI ───────────────────────────────────────────────────────
function renderUserStatusBadge(){
  const saved=localStorage.getItem("cox_status")||"online";
  if(saved!=="online")USER_STATUS=saved;
  const ds=document.querySelectorAll(".sidebar-status-dot");
  ds.forEach(d=>{d.className="sidebar-status-dot status-dot "+USER_STATUS;});
}
function openStatusMenu(e){
  const ex=document.getElementById("status-menu");
  if(ex){ex.remove();return;}
  const menu=document.createElement("div");menu.id="status-menu";menu.className="status-menu";
  menu.innerHTML=[
    {s:"online",icon:"ti ti-circle-check",label:"Online"},
    {s:"away",icon:"ti ti-clock",label:"Away"},
    {s:"busy",icon:"ti ti-circle-x",label:"Busy"},
  ].map(o=>`<button onclick="setUserStatus('${o.s}');document.getElementById('status-menu').remove();toasty('Status: ${o.label}','ok')"><i class="${o.icon}"></i>${o.label}</button>`).join("");
  const r=e.target.getBoundingClientRect();
  Object.assign(menu.style,{position:"fixed",left:r.left+"px",top:(r.bottom+4)+"px",zIndex:100});
  document.body.appendChild(menu);
  setTimeout(()=>document.addEventListener("click",()=>menu.remove(),{once:true}),10);
}

// ── Link unfurl ──────────────────────────────────────────────────────────
let UNFURL_CACHE={};
async function unfurlLinks(body){
  const urlRegex=/(https?:\/\/[^\s<]+)/g;const urls=[...body.matchAll(urlRegex)].map(m=>m[1]);
  if(!urls.length)return"";
  const unique=[...new Set(urls)].slice(0,3);
  const cards=[];
  for(const url of unique){
    if(UNFURL_CACHE[url]){cards.push(UNFURL_CACHE[url]);continue;}
    try{
      const r=await fetch("https://api.microlink.io?url="+encodeURIComponent(url));
      const d=await r.json();
      if(d.status==="success"){
        const card=`<a href="${esc(url)}" target="_blank" rel="noopener" class="unfurl-card">
          ${d.data.image?.url?`<img src="${esc(d.data.image.url)}" alt="">`:''}
          <div class="unfurl-body">
            <div class="unfurl-title">${esc(d.data.title||url)}</div>
            ${d.data.description?`<div class="unfurl-desc">${esc(d.data.description.substring(0,200))}</div>`:''}
            <div class="unfurl-url">${esc(d.data.publisher||new URL(url).hostname)}</div>
          </div></a>`;
        UNFURL_CACHE[url]=card;cards.push(card);
      }
    }catch(e){}
  }
  return cards.length?`<div class="unfurls">${cards.join("")}</div>`:"";
}
async function renderUnfurls(){
  const el=document.getElementById("unfurl-preview");if(!el)return;
  const inp=document.getElementById("chat-input");if(!inp)return;
  const body=inp.value.trim();if(!body){el.innerHTML="";el.style.display="none";return;}
  const html=await unfurlLinks(body);
  el.innerHTML=html;el.style.display=html?"block":"none";
}

// ── Channel topic ────────────────────────────────────────────────────────
async function loadChannelTopic(){
  const ch=getActiveChannel();if(!ch||ch==="general")return;
  try{
    const r=await fetch("/api/chat/channels/"+ch+"/topic");
    if(!r.ok)return;
    const d=await r.json();
    const el=document.getElementById("chat-topic");
    if(el)el.innerHTML=d.topic?`<span class="ch-topic" onclick="editChannelTopic()" title="Click to edit">${esc(d.topic)}</span>`:"";
  }catch(e){}
}
function editChannelTopic(){
  const el=document.getElementById("chat-topic");if(!el)return;
  const cur=el.textContent||"";
  el.innerHTML=`<input id="topic-edit" value="${esc(cur)}" style="background:var(--card);border:1px solid var(--border2);border-radius:6px;padding:2px 8px;font-size:11px;color:var(--text);width:200px" onkeydown="if(event.key==='Enter')saveTopic()" onblur="saveTopic()">`;
  document.getElementById("topic-edit").focus();}
async function saveTopic(){
  const inp=document.getElementById("topic-edit");if(!inp)return;
  const topic=inp.value.trim();const ch=getActiveChannel();
  try{await fetch("/api/chat/channel/"+ch+"/topic",{method:"PATCH",headers:{"Content-Type":"application/json"},body:JSON.stringify({topic})});
  }catch(e){}
  loadChannelTopic();
}

// ── Patch chatInputKey for slash ─────────────────────────────────────────
(function(){
  let origKey=chatInputKey;
  chatInputKey=function(e){
    if(e.key==="Enter"){
      const inp=document.getElementById("chat-input");
      if(inp&&inp.value.trim().startsWith("/")){
        if(handleSlashCommand())return;
      }
    }
    return origKey(e);
  };
  let origSend=sendChat;
  sendChat=function(){
    const inp=document.getElementById("chat-input");
    if(inp&&inp.value.trim().startsWith("/")){
      if(handleSlashCommand()){
        if(inp.value.trim()){origSend();}
        return;
      }
    }
    origSend();
  };
})();

// ── Init on mode switch ──────────────────────────────────────────────────
(function(){
  const orig=setMode;
  setMode=function(m,...args){
    orig(m,...args);
    if(m==="chat"){loadPins();loadChannelTopic();renderUserStatusBadge();renderChatList(true);}
  };
})();

// ── Settings helpers ────────────────────────────────────────────────────
async function saveProfile(){
  const name=document.getElementById("prof-name")?.value||"";
  const email=document.getElementById("prof-email")?.value||"";
  const pw=document.getElementById("prof-pw")?.value||"";
  const n=document.getElementById("prof-note");
  try{
    const r=await fetch("/api/auth/profile",{method:"PATCH",headers:{"Content-Type":"application/json"},body:JSON.stringify({name,email,password:pw||undefined})});
    if(r.ok){if(n)n.innerHTML='<span style="color:var(--green)">Profile updated</span>';}
    else{if(n)n.innerHTML=`<span style="color:var(--red)">${await r.text()}</span>`;}
  }catch(e){if(n)n.innerHTML='<span style="color:var(--red)">Network error</span>';}
}
function saveNotify(){
  ["ntf-desktop","ntf-sound"].forEach(id=>{const el=document.getElementById(id);if(el)localStorage.setItem("cox_"+id.replace("ntf-",""),el.value);});
  const m=document.getElementById("ntf-meet");if(m)localStorage.setItem("cox_meet_remind",m.value);
  toasty("Notification settings saved","ok");
}
function saveMeetings(){
  const d=document.getElementById("mtg-dur");if(d)localStorage.setItem("cox_mtg_dur",d.value);
  const r=document.getElementById("mtg-remind");if(r)localStorage.setItem("cox_mtg_remind",r.value);
  const ring=document.getElementById("mtg-ring");if(ring)localStorage.setItem("cox_mtg_ring",ring.value);
  toasty("Meeting defaults saved","ok");
}
function applyTheme(t){
  document.documentElement.classList.toggle("light",t==="light");
  localStorage.setItem("cox_theme",t);
}
function applyAccent(c){
  document.documentElement.style.setProperty("--accent2",c);
  localStorage.setItem("cox_accent",c);
}
function applyFontSize(s){
  document.documentElement.style.fontSize=s+"px";
  localStorage.setItem("cox_fs",s);
}
(function initThemeAppearance(){
  const t=localStorage.getItem("cox_theme")||"dark";
  document.documentElement.classList.toggle("light",t==="light");
  const a=localStorage.getItem("cox_accent");if(a)document.documentElement.style.setProperty("--accent2",a);
  const f=localStorage.getItem("cox_fs");if(f)document.documentElement.style.fontSize=f+"px";
})();

boot();
