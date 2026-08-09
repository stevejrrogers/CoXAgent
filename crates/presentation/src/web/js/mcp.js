// MCP self-service tokens.
// Split from index.html — classic script, load order matters (one shared scope).
// ---- MCP self-service (every signed-in user) ----------------------------
function mcpSnippets(token){const url=location.origin+"/api/mcp";const t=token||"<YOUR-TOKEN>";
  return [
    {name:"Claude Code",file:".mcp.json (repo root)",code:JSON.stringify({mcpServers:{coxagent:{type:"http",url,headers:{Authorization:"Bearer "+t}}}},null,2)},
    {name:"opencode",file:"opencode.json (repo root)",code:JSON.stringify({"$schema":"https://opencode.ai/config.json",mcp:{coxagent:{type:"remote",url,enabled:true,headers:{Authorization:"Bearer "+t}}}},null,2)},
    {name:"Other MCP clients",file:"streamable HTTP",code:`URL:    ${url}\nHeader: Authorization: Bearer ${t}`}
  ];}
async function renderMcpPanel(){const el=document.getElementById("mcp-panel");if(!el)return;
  let toks=[];try{const r=await fetch("/api/auth/my/tokens");if(r.ok)toks=await r.json();}catch(e){}
  const fresh=window._mcpFresh; // {label,token} — shown once after minting
  const tools=[["search_symbols","find code symbols across the project"],["symbol_refs","who calls / references a symbol"],["get_ticket","full ticket detail"],["pr_queue","open PRs + review state"],["report_blocker","file a blocker to the team"]];
  setHTML(el,`
    <div class="panel" style="margin-bottom:12px">
      <div style="font-size:13px;font-weight:700;margin-bottom:4px"><i class="ti ti-plug-connected" style="color:var(--accent2)"></i> CoXAgent MCP server</div>
      <div style="font-size:12px;color:var(--muted);margin-bottom:8px">Connect any MCP-capable agent (Claude Code, opencode, IDEs…) to this hub — it gets the code knowledge graph and team tools with your permissions.</div>
      <div style="display:flex;flex-wrap:wrap;gap:6px">${tools.map(([n,d])=>`<span class="tk" title="${esc(d)}" style="background:var(--card2);border:1px solid var(--border2)">${n}</span>`).join("")}</div>
    </div>
    <div class="panel" style="margin-bottom:12px">
      <div style="font-size:13px;font-weight:700;margin-bottom:6px"><i class="ti ti-key" style="color:var(--accent2)"></i> Your access tokens</div>
      ${fresh?`<div style="background:color-mix(in srgb,var(--green) 9%,transparent);border:1px solid color-mix(in srgb,var(--green) 33%,transparent);border-radius:8px;padding:9px 12px;margin-bottom:9px;font-size:12px">
        <b style="color:var(--green)">Token created.</b> Copy it now — it is not shown again.<br>
        <code style="user-select:all;word-break:break-all">${esc(fresh.token)}</code>
        <button class="toolcopy" onclick="copyText(this,'${esc(fresh.token)}')" title="Copy"><i class="ti ti-copy"></i></button></div>`:""}
      ${toks.length?toks.map(t=>`<div style="display:flex;align-items:center;gap:8px;font-size:12px;padding:4px 0;border-bottom:1px solid var(--border2)">
        <i class="ti ti-key" style="color:var(--dim)"></i><code style="flex:1">${esc(t.label)}</code>
        <span style="color:var(--dim)">${esc((t.created||"").slice(0,10))}</span>
        <button class="btn-ghost" style="color:var(--red)" onclick="revokeMyToken('${esc(t.label)}')" title="Revoke"><i class="ti ti-trash"></i></button></div>`).join("")
      :`<div style="font-size:12px;color:var(--dim);padding:2px 0 6px">No personal tokens yet — create one to connect your tools.</div>`}
      <div style="display:flex;gap:8px;margin-top:9px">
        <input id="mcp-tok-label" placeholder="label — e.g. laptop, vscode" style="width:220px;background:var(--card);color:var(--text);border:1px solid var(--border2);border-radius:8px;padding:7px 11px;font-size:12.5px"/>
        <button class="save" onclick="createMyToken()"><i class="ti ti-plus"></i> Create token</button>
      </div>
      <div style="font-size:11.5px;color:var(--dim);margin-top:6px">Tokens act as <b>you</b> with your current role — never more. Revoke any you no longer use.</div>
    </div>
    ${mcpSnippets(fresh&&fresh.token).map(s=>`<div class="panel" style="margin-bottom:12px">
      <div style="display:flex;align-items:center;gap:8px;margin-bottom:6px"><b style="font-size:12.5px">${esc(s.name)}</b><span style="font-size:11.5px;color:var(--dim)">${esc(s.file)}</span>
        <span style="flex:1"></span><button class="toolcopy" onclick='copyText(this,${JSON.stringify(s.code)})' title="Copy"><i class="ti ti-copy"></i></button></div>
      <pre style="margin:0;background:var(--card);border:1px solid var(--border2);border-radius:8px;padding:10px 12px;font-size:11.5px;overflow-x:auto">${esc(s.code)}</pre></div>`).join("")}`);
}
async function createMyToken(){const label=(document.getElementById("mcp-tok-label")||{}).value||"";
  if(!label.trim()){toasty("Enter a label first","err");return;}
  try{const r=await fetch("/api/auth/my/tokens",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({label})});
    if(r.ok){window._mcpFresh=await r.json();renderMcpPanel();}
    else toasty(await r.text()||"Could not create token","err");
  }catch(e){toasty("Network error","err");}}
async function revokeMyToken(label){
  try{const r=await fetch("/api/auth/my/tokens/"+encodeURIComponent(label),{method:"DELETE"});
    if(r.ok){if(window._mcpFresh&&window._mcpFresh.label===label)window._mcpFresh=null;toasty("Token revoked","ok");renderMcpPanel();}
    else toasty(await r.text()||"Could not revoke","err");
  }catch(e){toasty("Network error","err");}}
function toggleRoleOverrides(btn){const box=btn.nextElementSibling;const open=box.hasAttribute("hidden");
  if(open){box.removeAttribute("hidden");btn.classList.add("open");}else{box.setAttribute("hidden","");btn.classList.remove("open");}
}
async function saveSettings(){const cfg=window._cfg||{engine:{},workflow:{}};cfg.engine=cfg.engine||{};cfg.workflow=cfg.workflow||{};
  function mdl(id){const provEl=document.getElementById("mdl-prov-"+id);if(provEl)return provEl.value+"/"+(val("mdl-"+id)||"");return val("mdl-"+id)||"";}
  cfg.engine.default={engine:val("eng-default"),model:mdl("default")};
  const per={};ROLES.forEach(r=>{const e=val("eng-"+r),m=mdl(r);if(e)per[r]={engine:e,model:m||"sonnet"};});cfg.engine.per_role=per;
  cfg.engine.fallbacks=(val("eng-fallbacks")||"").split("\n").map(l=>l.trim()).filter(Boolean).map(l=>{const p=l.split(/\s+/);return{engine:p[0],model:p.slice(1).join(" ")||"sonnet"};}).filter(f=>f.engine);
  cfg.engine.auto_fallback=val("eng-autofb")!=="false";
  cfg.workflow.mode=val("wf-mode");
  cfg.workflow.sprint_length_cycles=parseInt(val("wf-sp")||"10",10);
  cfg.workflow.ba_every_n_cycles=parseInt(val("wf-ba")||"4",10);
  cfg.workflow.feature_dev_enabled=val("wf-fd")==="true";
  cfg.workflow.ops_monitor=val("wf-ops")!=="false";
  cfg.workflow.token_saver=val("wf-ts")==="true";
  cfg.workflow.sandbox=val("wf-sbx")==="true";
  cfg.workflow.tdd=val("wf-tdd")==="true";
  {const g=parseFloat(val("wf-gate"));cfg.workflow.approve_over_usd=Number.isFinite(g)&&g>0?g:null;}
  cfg.engine.escalation=val("en-esc").split(",").map(s=>s.trim()).filter(Boolean);
  cfg.workflow.language=val("wf-lang")||"en";
  cfg.workflow.sleep_seconds=parseInt(val("wf-sl")||"30",10);
  const bg=val("wf-bg");cfg.workflow.budget_usd=bg?parseFloat(bg):null;
  cfg.policy=cfg.policy||{};
  const dg=val("wf-dg");cfg.policy.daily_budget_usd=dg?parseFloat(dg):null;
  {const bwp=parseFloat(val("wf-bwp"));cfg.policy.budget_warn_pct=Number.isFinite(bwp)&&bwp>0?Math.min(bwp,100)/100:0.8;}
  cfg.git=Object.assign(cfg.git||{},{
    enabled:val("git-en")==="true",provider:val("git-pv"),repo:val("git-repo").trim(),
    base_url:val("git-url").trim(),default_branch:val("git-br").trim()||"main",
    target_branch:val("git-tb").trim(),
    branch_prefix:val("git-bp").trim()||"feat/",commit_email:val("git-em").trim(),
    account:val("git-acct").trim(),
    auto_pr:val("git-pr")==="true",auto_review:val("git-ar")==="true",auto_merge:val("git-am")==="true"});
  try{const res=await(await fetch(api("/config"),{method:"PUT",headers:{"Content-Type":"application/json"},body:JSON.stringify(cfg)})).json();
    window._budget=cfg.workflow.budget_usd;if(CUR==="insights")renderActive();
    document.getElementById("save-note").textContent=res.ok?(res.note||"saved"):"error";}catch(e){document.getElementById("save-note").textContent="error";}}
function val(id){return document.getElementById(id).value;}
async function setPriority(id,p){
  try{await fetch(api("/ticket/"+id+"/priority"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({priority:p})});
    const t=(STATE.tickets||[]).find(x=>x.id===id);if(t)t.priority=p;showTicket(id);}catch(e){}
}
function close_(id){document.getElementById(id).classList.remove("open");}
function setConn(on){document.getElementById("conn").className="dot "+(on?"live":"off");document.getElementById("connlbl").textContent=on?"live":"reconnecting";}
function renderRunner(r){if(!r)return;window.RUNNER=r;const running=r.mode==="running";const c=running?"live":(r.mode==="paused"?"paused":"off");
  const dot=document.getElementById("runmode");if(dot)dot.className="dot "+c;
  const lbl=document.getElementById("runlbl");
  if(lbl)lbl.textContent=running?(r.active_role?r.active_role.replace(/_/g,'-'):("cycle "+r.cycle)):(r.cycle>0?("paused · "+r.cycle):"idle");
  const pill=document.getElementById("runpill");if(pill)pill.title=(running&&r.active_note)?(r.active_role+" — "+r.active_note):(r.last_summary||(running?"running":(r.cycle>0?"paused":"idle")));
  // Primary toggles Start/Pause; it's the accent "go" button unless running.
  const prim=document.getElementById("ctl-primary"),ic=document.getElementById("ctl-primary-ic");
  if(prim&&ic){ic.className="ti ti-player-"+(running?"pause":"play");prim.title=running?"Pause":(r.cycle>0?"Resume":"Start");prim.classList.toggle("rp-go",!running);}
  const step=document.getElementById("ctl-step");if(step)step.style.display=running?"none":"";}
function toggleRun(){ctl((window.RUNNER&&RUNNER.mode==="running")?"pause":"resume");}
async function ctl(a){try{renderRunner(await(await fetch(api("/control/"+a),{method:"POST"})).json());}catch(e){}}
let PID=null, ES=null, poll=null;
const api=p=>"/api/projects/"+encodeURIComponent(PID)+p;
let ONLINE=[];
function handle(m){if(m.state)render(m.state);renderRunner(m.runner);setConn(true);
  if(Array.isArray(m.online)){ONLINE=m.online;updateOnline();}
  if(typeof m.viewers==="number"){const c=document.getElementById("viewers-chip");
    document.getElementById("viewers-n").textContent=m.viewers;c.style.display=m.viewers>1?"":"none";
    c.title=m.viewers+" user"+(m.viewers>1?"s":"")+" viewing (counts distinct people, not tabs)";}}
// Show how many members of the current channel are online (green dot + count).
function updateOnline(){
  const el=document.getElementById("chat-online");if(!el)return;
  const set=new Set(ONLINE);
  let members=channelMembers(); // usernames
  const me=(ME&&ME.username)||"user"; if(!set.has(me))set.add(me); // count self
  const n=members.filter(u=>set.has(u)).length || (currentChannel().kind==="general"?set.size:0);
  el.style.display=n>0?"inline-flex":"none";
  document.getElementById("chat-online-n").textContent=n;
  // Refresh the DM presence dots when the online set changes.
  const sig=[...set].sort().join(",");
  if(sig!==updateOnline._sig){updateOnline._sig=sig;renderDMList();}
}
// ── Documentation ───────────────────────────────────────────────────────────
let DOCS=[], DOC_CUR=null, DOC_EDIT=false;
function docCatLabel(c){return {product:"Product",technical:"Technical",flows:"Flows",qa:"Testing",ops:"Operations",general:"General"}[c]||c;}
// A doc's folder is a "/"-separated path (e.g. "Technical/Architecture").
// Legacy pages with no folder fall back to their category label.
function docFolder(d){return (d.folder&&d.folder.trim())||docCatLabel(d.category);}
const FOLDER_ICON={Product:"ti-package",Technical:"ti-code",Flows:"ti-route",Testing:"ti-checklist",Operations:"ti-server-cog"};
let DOC_FOLDERS=[],DOC_EXP=null;
function docExp(){if(!DOC_EXP){try{DOC_EXP=new Set(JSON.parse(localStorage.getItem("cox_docexp")||"[]"));}catch(e){DOC_EXP=new Set();}}return DOC_EXP;}
function saveExp(){localStorage.setItem("cox_docexp",JSON.stringify([...docExp()]));}
function toggleFolder(path){const s=docExp();if(s.has(path))s.delete(path);else s.add(path);saveExp();renderDocsList();}
async function loadDocs(){
  try{DOCS=await(await fetch(api("/docs"))).json();}catch(e){DOCS=[];}
  try{DOC_FOLDERS=await(await fetch(api("/doc-folders"))).json();}catch(e){DOC_FOLDERS=[];}
  if(DOC_CUR&&!DOCS.some(d=>d.id===DOC_CUR))DOC_CUR=null;
  if(!DOC_CUR&&DOCS.length)DOC_CUR=DOCS[0].id;
  renderDocsList();renderDocMain();
}
// Build a nested folder tree from explicit folders + each page's folder path.
function buildDocTree(){
  const root={name:"",path:"",folders:{},pages:[]};
  const ensure=path=>{if(!path)return root;let cur=root,acc="";for(const part of path.split("/")){if(!part)continue;acc=acc?acc+"/"+part:part;cur.folders[part]=cur.folders[part]||{name:part,path:acc,folders:{},pages:[]};cur=cur.folders[part];}return cur;};
  for(const f of (DOC_FOLDERS||[]))ensure(f);
  for(const d of DOCS)ensure((d.folder&&d.folder.trim())||"").pages.push(d);
  return root;
}
// Every folder path (for the "move page" picker).
function allFolderPaths(){const s=new Set(DOC_FOLDERS||[]);for(const d of DOCS){const f=(d.folder||"").trim();if(f){let acc="";for(const p of f.split("/")){acc=acc?acc+"/"+p:p;s.add(acc);}}}return [...s].sort();}
function renderDocsList(){
  const box=document.getElementById("docs-list");if(!box)return;
  const root=buildDocTree(),exp=docExp();
  const node=(n,depth)=>{let h="";
    for(const fn of Object.keys(n.folders).sort((a,b)=>a.localeCompare(b))){const f=n.folders[fn];const open=exp.has(f.path);const pad=8+depth*13;
      h+=`<div class="docfolder" style="padding-left:${pad}px" onclick="toggleFolder('${esc(f.path)}')">
        <i class="ti ti-chevron-${open?'down':'right'} dfc"></i><i class="ti ${FOLDER_ICON[f.name]||'ti-folder'} dfi"></i>
        <span class="dfn">${esc(f.name)}</span>
        <span class="dfacts">
          <i class="ti ti-file-plus" title="New page here" onclick="event.stopPropagation();newDoc('${esc(f.path)}')"></i>
          <i class="ti ti-folder-plus" title="New subfolder" onclick="event.stopPropagation();newFolder('${esc(f.path)}')"></i>
          <i class="ti ti-trash" title="Delete folder" onclick="event.stopPropagation();deleteFolder('${esc(f.path)}')"></i>
        </span></div>`;
      if(open)h+=node(f,depth+1);
    }
    for(const d of n.pages.slice().sort((a,b)=>a.title.localeCompare(b.title))){const pad=8+depth*13+18;
      h+=`<div class="docitem${d.id===DOC_CUR?' on':''}" style="padding-left:${pad}px" onclick="openDoc('${esc(d.id)}')"><i class="ti ti-file-text dfi" style="opacity:.55"></i> ${esc(d.title)}</div>`;
    }
    return h;};
  box.innerHTML=node(root,0)||'<div class="empty" style="padding:14px 8px;font-size:12px">Empty. Create a folder (＋) or page.</div>';
}
async function newFolder(parent){
  const name=await coxModal({title:"New folder",message:parent?("Tạo folder trong \""+parent+"\"."):"Tạo folder ở root.",input:{placeholder:"Folder name"},confirmText:"Create"});if(!name||!name.trim())return;
  const path=(parent?parent+"/":"")+name.trim().replace(/\//g,"-");
  try{await fetch(api("/doc-folders"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({path})});}catch(e){}
  if(parent)docExp().add(parent);docExp().add(path);saveExp();await loadDocs();
}
async function deleteFolder(path){if(!await coxModal({title:"Delete folder",message:'Xoá folder "'+path+'" và toàn bộ nội dung bên trong? Không hoàn tác được.',danger:true,confirmText:"Delete"}))return;
  try{await fetch(api("/doc-folders/delete"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({path})});}catch(e){}
  await loadDocs();
}
async function moveDoc(id){const cur=DOCS.find(d=>d.id===id);if(!cur)return;
  const folder=await coxModal({title:"Move page",message:"Folder đích (để trống = root).",input:{placeholder:"e.g. Technical/Architecture",value:cur.folder||""},confirmText:"Move"});if(folder===null)return;
  try{await fetch(api("/docs/"+id+"/move"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({path:folder.trim()})});}catch(e){}
  if(folder.trim())docExp().add(folder.trim());saveExp();await loadDocs();}
function openDoc(id){if(typeof docsWsClose==="function")docsWsClose();DOC_CUR=id;DOC_EDIT=false;renderDocsList();renderDocMain();}
function currentDoc(){return DOCS.find(d=>d.id===DOC_CUR);}
// Distinct folder paths already in use, for the editor's datalist.
function knownFolders(){return [...new Set(DOCS.map(docFolder))].sort();}
function renderDocMain(){
  const el=document.getElementById("docs-main");if(!el)return;
  const d=currentDoc();
  if(!d){el.innerHTML='<div class="docs-empty"><i class="ti ti-book-2"></i><div>No page selected</div><span>Generate docs with the DOCS agent (✨) or add a page (＋).</span></div>';return;}
  const canEdit=!(ME&&ME.auth)||roleCanWrite(ME.role);
  if(DOC_EDIT){
    const opts=knownFolders().map(f=>`<option value="${esc(f)}">`).join("");
    const tb=(cmd,ic,ti,arg)=>`<button type="button" title="${ti}" onmousedown="event.preventDefault()" onclick="${cmd}"><i class="ti ${ic}"></i></button>`;
    el.innerHTML=`<div class="docedit">
      <input id="doc-title" class="doc-t" value="${esc(d.title)}" placeholder="Title" oninput="rtDirty()">
      <input id="doc-folder" class="doc-t" style="font-size:13px;color:var(--dim)" list="doc-folders" value="${esc(docFolder(d))}" placeholder="Folder (e.g. Technical/Architecture)" oninput="rtDirty()">
      <datalist id="doc-folders">${opts}</datalist>
      <div class="rt-tools">
        ${tb("rtBlock('h1')","ti-h-1","Heading 1")}${tb("rtBlock('h2')","ti-h-2","Heading 2")}${tb("rtBlock('h3')","ti-h-3","Heading 3")}${tb("rtBlock('p')","ti-pilcrow","Paragraph")}
        <span class="sep"></span>
        ${tb("rtCmd('bold')","ti-bold","Bold")}${tb("rtCmd('italic')","ti-italic","Italic")}${tb("rtCmd('strikeThrough')","ti-strikethrough","Strikethrough")}${tb("rtInlineCode()","ti-code","Inline code")}
        <span class="sep"></span>
        ${tb("rtCmd('insertUnorderedList')","ti-list","Bullet list")}${tb("rtCmd('insertOrderedList')","ti-list-numbers","Numbered list")}${tb("rtChecklist()","ti-checkbox","Task list")}${tb("rtBlock('blockquote')","ti-quote","Quote")}
        <span class="sep"></span>
        ${tb("rtCodeBlock()","ti-source-code","Code block")}${tb("rtTable()","ti-table","Table")}${tb("rtLink()","ti-link","Link")}${tb("rtHr()","ti-separator-horizontal","Divider")}
      </div>
      <div id="doc-rich" class="rt-edit" contenteditable="true" data-empty="Start writing… use the toolbar to format."></div>
      <div class="doc-actions">
        <button class="pri" onclick="saveDoc()"><i class="ti ti-check"></i> Done</button>
        <button class="gc-btn" onclick="cancelEdit()">Cancel</button>
        <span id="rt-status" class="rt-saved"></span>
        <span id="rt-people" class="rt-presence" style="margin-left:auto"></span>
        <button class="gc-btn" style="color:var(--red)" onclick="deleteDoc()"><i class="ti ti-trash"></i> Delete</button>
      </div>
    </div>`;
    const rich=document.getElementById("doc-rich");
    rich.innerHTML=mdRender(d.body)||"<p><br></p>";
    docEditInit(d);
    return;
  }
  const meta=d.updated_by?`<span class="doc-meta">Updated by ${esc(d.updated_by)}${d.updated_at?" · "+esc(d.updated_at.slice(0,10)):""}</span>`:"";
  const crumb=docFolder(d).split("/").map(esc).join(' <i class="ti ti-chevron-right" style="font-size:11px;opacity:.5"></i> ');
  const actions=canEdit?`<div style="display:flex;gap:6px"><button class="gc-btn" onclick="askAiEdit()" title="Ask the DOCS agent to revise this page"><i class="ti ti-sparkles"></i> Ask AI</button><button class="gc-btn" onclick="moveDoc('${esc(d.id)}')" title="Move to another folder"><i class="ti ti-folder-symlink"></i> Move</button><button class="gc-btn" onclick="DOC_EDIT=true;renderDocMain()"><i class="ti ti-pencil"></i> Edit</button></div>`:"";
  el.innerHTML=`<div class="doc-head"><div><span class="doc-cat-tag ${esc(d.category)}">${crumb}</span><h2>${esc(d.title)}</h2>${meta}</div>${actions}</div><div class="doc-body md">${mdRender(d.body)}</div>`;
}
async function newDoc(folder){
  const title=await coxModal({title:"New page",message:"Tiêu đề trang mới.",input:{placeholder:"Page title"},confirmText:"Next"});if(!title||!title.trim())return;
  // Folder given (from a tree node) → use it; else ask.
  if(folder===undefined||folder===null){folder=(await coxModal({title:"New page",message:"Folder chứa trang (để trống = root).",input:{placeholder:"e.g. Product, Technical/Architecture"},confirmText:"Create"}));if(folder===null)return;folder=folder.trim();}
  if(folder)docExp().add(folder),saveExp();
  await putDoc("", folder, title.trim(), "# "+title.trim()+"\n\nWrite here…");DOC_EDIT=true;renderDocMain();
}
