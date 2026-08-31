// Shared helpers, overview charts, roadmap/delivery timeline, board columns.
// Split from index.html — classic script, load order matters (one shared scope).
const AGENTS=[["BA","proposes features","--blue"],["PO","owns priority","--amber"],["SM","runs the process","--teal"],
  ["SA","designs the how","--purple"],["PD","owns UX","--purple"],["DEV-BUG","fixes bugs","--red"],
  ["DEV-FEATURE","builds features","--green"],["TEST","verifies work","--teal"],["DOCS","writes guides","--blue"]];
const FCOLS=[["pending","Pending","--muted"],["ready","Ready","--blue"],["in_progress","In progress","--amber"],["done","Done","--green"],["documented","Documented","--teal"]];
const BCOLS=[["open","Open","--red"],["fixed","Fixed","--amber"],["verified","Verified","--green"]];
// Unified board: one lifecycle for features + bugs, each column collecting both.
const UCOLS=[
  ["backlog","Backlog","--muted",["pending","open","on_hold"]],
  ["ready","Ready","--accent2",["ready"]],
  ["in_progress","In Progress","--amber",["in_progress"]],
  ["done","Done","--green",["done","fixed"]],
  ["shipped","Shipped","--teal",["documented","verified"]],
];
const ROLES=["ba","po","sm","sa","pd","dev_bug","dev_feature","test","docs"];
const MODELS={claude:["sonnet","opus","haiku"],copilot:["auto","claude-sonnet-4.6","claude-sonnet-4.5","claude-haiku-4.5","gpt-5.4","gpt-5.4-mini","gpt-5.3-codex","gemini-3.1-pro-preview","grok-4.5"],scripted:["n/a"],mock:["n/a"],opencode:null,hermes:["hermes-3-llama-3.1-70b","hermes-2-pro-mistral-7b"],gemini:["gemini-2.5-pro","gemini-2.5-flash","gemini-2.0-flash"],codex:["gpt-4o","gpt-5","gpt-4"]};
// Common opencode provider/model choices (it accepts any, incl. local ollama).
const OPENCODE_PROVIDERS=[{id:"anthropic",label:"Anthropic"},{id:"openai",label:"OpenAI"},{id:"google",label:"Google"},{id:"openrouter",label:"OpenRouter"},{id:"groq",label:"Groq"},{id:"deepseek",label:"DeepSeek"},{id:"ollama",label:"Ollama (local)"},{id:"mistral",label:"Mistral"}];
let OC_MODELS=["claude-sonnet-4-5","claude-opus-4-1","gpt-5","gpt-4o","gemini-2.5-pro","gemini-2.5-flash","llama3.1","qwen2.5-coder","mixtral-8x7b","deepseek-v3","deepseek-r1"];
let OC_PROVIDERS=OPENCODE_PROVIDERS.map(p=>({...p}));
async function loadOpencodeModels(){
  if(!ME?.auth)return;
  try{const r=await fetch("/api/engines/opencode/models");const list=await r.json();
    if(Array.isArray(list)&&list.length){
      const provs=new Map(); const models=new Set();
      list.forEach(m=>{
        if(m.provider&&m.model){
          provs.set(m.provider,{id:m.provider,label:m.provider.charAt(0).toUpperCase()+m.provider.slice(1)});
          models.add(m.model);
        }
      });
      if(provs.size>0){
        OC_PROVIDERS=[...provs.values()];
        OC_MODELS=[...models];
        console.log("Detected "+OC_PROVIDERS.length+" opencode providers with "+OC_MODELS.length+" models");
      }
  }}catch(e){/* will retry on settings load */}
}
setTimeout(()=>{if(ME?.auth)loadOpencodeModels();},2000);
function modelControl(eng,cur,sid){
  if(!eng)return `<input id="mdl-${sid}" value="" placeholder="uses default model" disabled style="flex:1;opacity:.45"/>`;
  const list=MODELS[eng];
  if(!list){ // opencode: provider picker + model with datalist
    const providers=OC_PROVIDERS.length?OC_PROVIDERS:OPENCODE_PROVIDERS;
    const models=OC_MODELS.length?OC_MODELS:["sonnet","opus","gpt-4o"];
    const [curProv,curModel]=(cur||"").includes("/")?cur.split("/",2):[providers[0].id,(cur||models[0])];
    return `<select id="mdl-prov-${sid}" onchange="opencodeModelChange('${sid}')" style="width:140px;flex:none">`+
      providers.map(p=>`<option value="${p.id}" ${p.id===curProv?'selected':''}>${p.label}</option>`).join("")+
      `</select>
      <input id="mdl-${sid}" value="${esc(curModel)}" list="opencode-models-${sid}" placeholder="model name" style="flex:1;min-width:140px"/>
      <datalist id="opencode-models-${sid}">${models.map(m=>`<option value="${esc(m)}">`).join("")}</datalist>
      <span style="font-size:10px;color:var(--dim);padding:0 4px;white-space:nowrap">= ${esc(curProv)}/${esc(curModel)}</span>`;
  }
  const opts=[...list]; if(cur&&!opts.includes(cur))opts.unshift(cur);
  return `<select id="mdl-${sid}" style="flex:1">${opts.map(m=>`<option ${m===cur?'selected':''}>${esc(m)}</option>`).join("")}</select>`;
}
function opencodeModelChange(sid){
  const prov=document.getElementById("mdl-prov-"+sid)?.value;
  const model=document.getElementById("mdl-"+sid)?.value||"";
  const hint=document.querySelector("#mc-"+sid+" span");
  if(hint)hint.textContent="= "+prov+"/"+model;
}
function onEngine(sid){const eng=document.getElementById("eng-"+sid).value;
  const list=MODELS[eng];const def=(list&&list.length)?list[0]:(OC_PROVIDERS[0]?.id||"anthropic")+"/"+(OC_MODELS[0]||"sonnet");
  document.getElementById("mc-"+sid).innerHTML=modelControl(eng,def,sid);}
const KPI_IC={Shipped:"ti-rocket","In flight":"ti-plane-tilt","Open bugs":"ti-bug",Documented:"ti-book",Releases:"ti-versions",Cost:"ti-coin","Total spend":"ti-coin",Tokens:"ti-cpu",Runs:"ti-repeat","Agent actions":"ti-bolt","Tickets shipped":"ti-rocket","Bugs open":"ti-bug","Team cost":"ti-coin"};
const money=n=>"$"+(Number(n)||0).toFixed(2);
const fmtK=n=>{n=Number(n)||0;return n>=1e9?(n/1e9).toFixed(1)+"B":n>=1e6?(n/1e6).toFixed(1)+"M":n>=1000?(n/1000).toFixed(1)+"k":String(n);};
const AC={BA:"--blue","DEV-FEATURE":"--green","DEV-BUG":"--red",SA:"--purple",TEST:"--teal",DOCS:"--blue",PO:"--amber",SM:"--teal",PD:"--purple",USER:"--accent"};
// Stable nicknames so each role reads as one consistent person, not a label.
const AGENT_NICK={BA:"Bella",PO:"Pola",SM:"Sam",SA:"Aria","DEV-FEATURE":"Finn","DEV-BUG":"Bex",TEST:"Quinn",DOCS:"Dana",PD:"Piper"};
const TITLES={"mg-spaces":["Spaces","every team space in the hub"],"mg-space":["Space","deep dive"],"mg-users":["Users","everyone across the hub"],"mg-usage":["Usage","who burns what"],"mg-audit":["Audit","every action across the hub"],home:["Home","your company · projects · your agents"],river:["Fleet river","every agent, every project — one live stream"],overview:["Overview","project health at a glance"],team:["Agents","your autonomous workers"],board:["Work","board · sprint · backlog"],inbox:["Inbox","everything waiting on YOU — approve · verify · answer"],activity:["Transcripts & alerts","per-run transcripts · outbound alerts · audit export — this project"],roadmap:["Roadmap","now · next · later, auto-generated"],discuss:["Scrum","standups, sprint events & team threads"],docs:["Wiki","product & technical knowledge base"],codemap:["Code map","files · symbols · dependencies the agents navigate"],calendar:["Calendar","meetings · schedule"],terminal:["Terminal","real shell in the project codebase — admin only"],chat:["Chat","talk with your teammates"],review:["Review","open pull requests — approve & merge"],people:["People","per-user activity & productivity"],audit:["Audit","who did what, when"],access:["Users","accounts, project access & tokens"],insights:["Cost","token spend across the team"],settings:["Settings","engines, models, workflow"]};
const esc=s=>(s||"").replace(/[&<>]/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;"}[c]));
// Engine/model provenance (CXA-F257): one attempt as the verify surfaces
// render it — JS mirror of application::engine_provenance::attempt_label, so
// an engine that cannot report a model id shows the explicit "model unknown"
// marker instead of a blank field, identically on card and detail.
const provLabel=a=>`${a.engine} · ${a.model&&a.model.trim()?a.model:"model unknown"}`;
const provChip=p=>`<span style="font-family:ui-monospace,Menlo,monospace;font-size:11.5px;font-weight:600;background:var(--card);border:1px solid var(--border2);border-radius:20px;padding:2px 8px;color:${p.model&&p.model.trim()?"var(--muted)":"var(--amber)"};white-space:nowrap">${esc(provLabel(p))}</span>`;
const cvar=n=>getComputedStyle(document.documentElement).getPropertyValue(n).trim()||"#888";
let STATE={}, CUR="overview", BF="all", SF="all", INIT_ACT=false, WORKTAB="board";
function setWorkTab(w){WORKTAB=w;
  document.querySelectorAll("#work-seg button").forEach(b=>b.classList.toggle("on",b.dataset.w===w));
  document.getElementById("work-board").style.display=w==="board"?"":"none";
  document.getElementById("work-sprint").style.display=w==="sprint"?"":"none";
  document.getElementById("work-backlog").style.display=w==="backlog"?"":"none";
  renderActive();}

function nav(v){
  // An unknown view (stale hash, typo, removed screen) used to blank the
  // whole pane: every view lost `on` and THEN getElementById(null) threw.
  // Fall back to overview instead of dying mid-switch.
  if(!document.getElementById("view-"+v))v="overview";
  // Manage views live in manage mode; everything else in workspace mode.
  if(String(v).startsWith("mg-")&&MODE!=="manage"){MODE="manage";localStorage.setItem("cox_mode",MODE);document.body.classList.add("mode-manage");document.body.classList.remove("mode-chat");}
  if(!String(v).startsWith("mg-")&&MODE==="manage"){MODE="workspace";localStorage.setItem("cox_mode",MODE);document.body.classList.remove("mode-manage");}
  if(isChatMode())setMode("workspace");
  // User administration is Admin + lead only — bounce members to Overview.
  // Settings is open to everyone (members see the self-service MCP tab).
  if(v==="access"&&!canManage())v="overview";
  CUR=v;location.hash=v;
  // The fleet river owns an SSE stream only while it is on screen.
  if(v!=="river"&&typeof closeFleetRiver==="function")closeFleetRiver();
  document.querySelectorAll(".view").forEach(x=>x.classList.remove("on"));
  document.getElementById("view-"+v).classList.add("on");
  document.querySelectorAll(".nav a").forEach(a=>a.classList.toggle("on",a.dataset.v===v));
  document.getElementById("pg-title").textContent=TITLES[v][0];
  document.getElementById("pg-sub").textContent=TITLES[v][1];
  try{updateSegments();}catch(e){}
   if(v==="settings")loadSettings(); else if(v==="calendar"){if(!Array.isArray(MEETINGS))MEETINGS=[];loadMeetings().then(renderCalendar).catch(()=>{MEETINGS=[];renderCalendar();});} else if(v==="discuss"){loadComments();} else if(v==="docs"){loadDocs();} else if(v==="people"){renderPeople();} else if(v==="audit"){renderAudit();} else if(v==="access"){renderAccess();} else if(v==="roadmap"){renderRoadmap();} else if(v==="review"){renderReview();} else if(v==="inbox"){renderInbox();} else if(v==="codemap"){renderCodeMap();} else if(v==="team"){loadAgentEvals();renderActive();} else if(v==="river"){openFleetRiver();} else if(v==="terminal"){openTerminal();} else renderActive();
   setTimeout(centerContent,50);}
function initials(r){return r.replace("DEV-","").slice(0,2);}
// A hostname as a person would say it: "Lutons-MacBook-Pro.local" -> "MacBook
// Pro". Drops the mDNS suffix, the dashes, and the owner's own name, which the
// account beside it already said.
function prettyHost(h,account){
  let s=String(h||"").replace(/\.local\.?$/i,"").replace(/[-_]+/g," ").trim();
  const a=String(account||"").trim();
  if(a){const own=new RegExp("^"+a.replace(/[.*+?^${}()|[\]\\]/g,"\\$&")+"(?:'?s)?\\s+","i");s=s.replace(own,"");}
  return s.trim()||String(h||"");
}
// Label a worker id (`account@host`) for display. The machine is named only
// when it actually tells two workers apart — always printing it makes every row
// longer in the common case (one person, one machine) while saying nothing.
function workerLabel(who,all){
  const s=String(who||""); const at=s.indexOf("@");
  if(at<0)return s;
  const account=s.slice(0,at), host=s.slice(at+1);
  const hosts=new Set((all||[]).map(String)
    .filter(w=>w.slice(0,w.indexOf("@"))===account&&w.includes("@"))
    .map(w=>w.slice(w.indexOf("@")+1)));
  return hosts.size>1?account+" · "+prettyHost(host,account):account;
}
function metricsFrom(s){const t=s.tickets||[],h=s.history||[],isF=x=>x.type!=="bug";
  return {shipped:t.filter(x=>isF(x)&&(x.status==="done"||x.status==="documented")).length,
    inflight:t.filter(x=>isF(x)&&(x.status==="ready"||x.status==="in_progress")).length,
    openBugs:t.filter(x=>x.type==="bug"&&x.status==="open").length,
    docd:t.filter(x=>x.status==="documented").length,releases:h.length};}
// Write innerHTML only when the content actually changed — the live refresh
// repaints every few seconds, and identical rewrites restart CSS animations
// (bars, pulses) making the whole page judder.
function setHTML(el,html){if(!el)return;if(el.__h!==html){el.__h=html;el.innerHTML=html;}}
function kpi(k,v,sub){return `<div class="kpi"><div class="ic"><i class="ti ${KPI_IC[k]||'ti-point'}"></i></div><div class="v">${v}</div><div class="k">${k}${sub?` <span style="color:var(--green);font-weight:600">· ${sub}</span>`:""}</div></div>`;}
function alertsHtml(s,m,spend){
  const al=[];
  if(s.deploy&&!s.deploy.ok)al.push(["red","cloud-x","Last deployment failed",esc(s.deploy.summary)]);
  const bud=window._budget;
  if(bud&&spend.total_cost_usd>=bud)al.push(["red","alert-triangle","Budget cap reached",money(spend.total_cost_usd)+" of "+money(bud)+" — loop paused"]);
  else if(bud&&spend.total_cost_usd>=bud*0.8)al.push(["amber","alert-triangle","Budget nearly reached",Math.round(spend.total_cost_usd/bud*100)+"% of "+money(bud)]);
  if(m.openBugs>=5)al.push(["amber","bug",m.openBugs+" open bugs","DEV-BUG is prioritising fixes over features"]);
  // Merged-then-reverted work (CXA-F047): shipped value that did not stick.
  // Amber while detections wait on a human; red once confirmed — a fact, not
  // a suspicion, and planning discounts it next cycle.
  const rv=s.reverted_work||[];
  if(rv.length){
    const pend=rv.filter(e=>e.decision==="pending").length,okd=rv.length-pend;
    if(pend)al.push(["amber","arrow-back-up","Reverted work",pend+" detected revert"+(pend===1?" needs":"s need")+" your review — decide in the Inbox"]);
    else if(okd)al.push(["red","arrow-back-up","Reverted work",okd+" confirmed revert"+(okd===1?"":"s")+" — planning weights them down next cycle"]);
  }
  if(!al.length)return "";
  return al.map(([c,ic,t,d])=>`<div class="panel" style="margin-bottom:12px;display:flex;align-items:center;gap:12px;border-color:var(--${c})">
    <i class="ti ti-${ic}" style="font-size:20px;color:var(--${c})"></i>
    <div><div style="font-size:13px;font-weight:600">${t}</div><div style="font-size:12px;color:var(--muted)">${d}</div></div></div>`).join("");
}
function velocityHtml(sprints){
  if(!sprints.length)return "";
  const max=Math.max(...sprints.map(s=>s.committed),1);
  const bars=sprints.slice(-10).map(s=>{const h=Math.max(6,Math.round(s.done/max*90));const hc=Math.max(6,Math.round(s.committed/max*90));
    return `<div style="display:flex;flex-direction:column;align-items:center;gap:6px;flex:1">
      <div style="position:relative;height:96px;width:26px;display:flex;align-items:flex-end">
        <div style="position:absolute;bottom:0;width:100%;height:${hc}px;background:var(--card2);border-radius:4px"></div>
        <div style="position:absolute;bottom:0;width:100%;height:${h}px;background:var(--accent2);border-radius:4px"></div></div>
      <div style="font-size:10px;color:var(--dim)">#${s.number}</div></div>`;}).join("");
  return `<div class="sec">Velocity</div><div class="panel"><div style="display:flex;gap:10px;align-items:flex-end">${bars}</div>
    <div style="color:var(--dim);font-size:11px;margin-top:10px">shipped (cyan) vs committed (grey) per closed sprint</div></div>`;
}
// Human governance-attention ledger (CXA-F230): where the operator's own
// review effort goes, per ticket class and gate kind. Reads the analytics
// response the backend already computes (60s cache, keyed by project — the
// same pattern the token-saver panel uses), because the raw ledger never
// rides the 1 Hz state snapshot. Zero gates render NOTHING: until a first
// decision lands, the overview reads exactly as before.
function loadGovernanceAttention(){
  // The cache is keyed by project: switching projects must never show the
  // previous project's attention data for the rest of the cache window.
  if(window._govPid!==PID){window._gov=null;window._govAt=0;}
  if(window._govAt&&Date.now()-window._govAt<60000){renderGovernanceAttention();return;}
  window._govPid=PID;window._govAt=Date.now();
  fetch(api("/metrics/summary")).then(r=>r.json()).then(d=>{
    window._gov=(d&&d.attention)?d.attention:null;
    if(CUR==="overview")renderGovernanceAttention();
  }).catch(()=>{window._gov=null;});
}
function renderGovernanceAttention(){
  const el=document.getElementById("ov-attention");if(!el)return;
  const a=window._gov;
  if(!a||!a.interventions_total){setHTML(el,"");return;}
  const kinds={ready_approve:"ready",verify_pass:"verify ✓",verify_send_back:"verify ↩",cost_approve:"cost",human_pr_reviewed:"PR landed",human_pr_dismissed:"PR dismissed",undo_auto_approve:"undo approval"};
  const rows=Object.entries(a.attention_by_area||{}).map(([area,counts])=>({
    area,total:Object.values(counts||{}).reduce((x,y)=>x+(y||0),0),counts:counts||{}
  })).sort((x,y)=>y.total-x.total);
  const max=Math.max(1,...rows.map(r=>r.total));
  const bar=r=>{
    const w=Math.max(3,Math.round(r.total/max*100));
    const tip=Object.entries(r.counts).filter(([,v])=>v).map(([k,v])=>(kinds[k]||k)+": "+v).join(" · ")||"no attributed decisions";
    return `<div style="display:flex;align-items:center;gap:12px;padding:8px 0">
      <span style="min-width:70px;font-size:12px;text-transform:capitalize">${esc(r.area)}</span>
      <div style="flex:1;background:var(--card2);border-radius:6px;height:8px;overflow:hidden" title="${esc(tip)}"><div style="width:${w}%;height:100%;background:var(--accent)"></div></div>
      <span style="font-size:12px;font-family:ui-monospace,monospace;min-width:30px;text-align:right">${r.total}</span></div>`;
  };
  const anomaly=a.anomaly?`<div style="display:flex;align-items:flex-start;gap:10px;margin-top:10px;padding:9px 12px;border:1px solid var(--border2);border-left:3px solid var(--amber);border-radius:10px;background:var(--card)">
    <i class="ti ti-alert-triangle" style="color:var(--amber);font-size:15px"></i>
    <div style="font-size:12px;color:var(--muted)"><b style="color:var(--text);text-transform:capitalize">${esc(a.anomaly.area)}</b> governance attention spiked — ${a.anomaly.recent_interventions} decisions in 3 days vs ${Number(a.anomaly.baseline_mean).toFixed(1)}/day trailing, with 0 verified tickets of that class in 14 days. Tune the gate, don't just enforce it.</div></div>`:"";
  setHTML(el,`<div class="sec" style="margin-top:22px">Governance attention <span style="font-size:11px;color:var(--dim);font-weight:400">· your own review effort by ticket class — ${a.interventions_total} gate decision${a.interventions_total===1?"":"s"} recorded</span></div>
    <div class="panel">
      ${rows.map(bar).join("")}
      ${a.unattributed?`<div style="font-size:11px;color:var(--dim);margin-top:8px"><i class="ti ti-eye-off"></i> ${a.unattributed} unattributed — decisions with no resolvable ticket class, counted but never guessed</div>`:""}
      ${anomaly}
    </div>`);
}
function chartsHtml(s){
  const ts=s.tickets||[],h=s.history||[];
  if(!ts.length&&!h.length)return "";
  // Ticket status distribution (stacked bar).
  const order=[["pending","--muted"],["ready","--accent2"],["in_progress","--amber"],["done","--green"],["documented","--teal"],["open","--red"],["fixed","--amber"],["verified","--green"],["rejected","--dim"]];
  const counts={};ts.forEach(t=>counts[t.status]=(counts[t.status]||0)+1);
  const total=ts.length||1;
  const seg=order.filter(([k])=>counts[k]).map(([k,c])=>`<div title="${k}: ${counts[k]}" style="width:${counts[k]/total*100}%;background:var(${c})"></div>`).join("");
  const legend=order.filter(([k])=>counts[k]).map(([k,c])=>`<span style="display:inline-flex;align-items:center;gap:5px;font-size:11px;color:var(--muted);margin-right:12px"><span style="width:9px;height:9px;border-radius:2px;background:var(${c})"></span>${k} ${counts[k]}</span>`).join("");
  const statusChart=`<div class="panel"><h4><i class="ti ti-chart-pie" style="color:var(--accent2)"></i> Ticket status</h4>
    <div style="display:flex;height:14px;border-radius:7px;overflow:hidden;background:var(--card2);margin:6px 0 10px">${seg||'<div style="width:100%"></div>'}</div>
    <div style="line-height:1.9">${legend||'<span class="empty">no tickets</span>'}</div></div>`;
  // Throughput: deliveries per day over the last 14 days.
  const days=[];for(let i=13;i>=0;i--){const d=new Date(Date.now()-i*86400000);days.push(d.toISOString().slice(0,10));}
  const perDay={};h.forEach(r=>{const d=(r.at||"").slice(0,10);perDay[d]=(perDay[d]||0)+1;});
  const max=Math.max(1,...days.map(d=>perDay[d]||0));
  const bars=days.map(d=>{const v=perDay[d]||0;const ht=Math.max(3,Math.round(v/max*70));
    return `<div title="${d}: ${v} shipped" style="flex:1;display:flex;flex-direction:column;justify-content:flex-end;align-items:center;gap:3px">
      <div style="width:70%;height:${ht}px;background:${v?'var(--accent2)':'var(--card2)'};border-radius:3px 3px 0 0"></div></div>`;}).join("");
  const totalShipped=h.length;
  const throughput=`<div class="panel"><h4><i class="ti ti-chart-bar" style="color:var(--accent2)"></i> Throughput <span style="font-size:11px;color:var(--dim);font-weight:400">· ${totalShipped} shipped, last 14 days</span></h4>
    <div style="display:flex;align-items:flex-end;height:80px;gap:2px;margin-top:6px">${bars}</div></div>`;
  return statusChart+throughput;
}
// Merged Gantt + milestones on a real time axis with quarter/week gridlines:
// each milestone is a bar (start → its target-version delivery date), releases
// are markers, and future targets are projected from shipping velocity.
function vnum(v){const p=String(v||"0.0.0").split(".").map(Number);return (p[0]||0)*10000+(p[1]||0)*100+(p[2]||0);}
function roadmapGantt(s){
  const ms=s.milestones||[];if(!ms.length)return "";
  const proj=(PROJECTS.find(p=>p.id===PID)||{}).name||PID;
  const hist=(s.history||[]).map(r=>({v:r.version,title:r.title,t:Date.parse(r.at)})).filter(r=>!isNaN(r.t)).sort((a,b)=>a.t-b.t);
  const now=Date.now(),cur=vnum(s.current_version);
  const DAY=864e5;
  // ms per numeric version-unit (a minor bump = 100 units), floored so future
  // targets still spread out even for fast-shipping projects.
  let perUnit=3*DAY/100;
  if(hist.length>=2){const span=hist[hist.length-1].t-hist[0].t,steps=Math.max(1,vnum(hist[hist.length-1].v)-vnum(hist[0].v));perUnit=Math.max(perUnit,span/steps);}
  const relDate=tv=>{const n=vnum(tv);const past=hist.filter(r=>vnum(r.v)<=n);return past.length?past[past.length-1].t:null;};
  const t0=hist.length?hist[0].t:now;
  let prev=t0,firstActive=true;
  const rows=ms.map(m=>{const reached=cur>=vnum(m.target_version);
    const end=reached?(relDate(m.target_version)||now):now+(vnum(m.target_version)-cur)*perUnit;
    const start=Math.min(prev,end);prev=end;
    let st="planned";if(reached)st="reached";else if(firstActive){st="active";firstActive=false;}
    return {name:m.name,goal:m.goal,ver:m.target_version,start,end,st};});
  // NOTE: this is an ordered stepper, not a time-positioned chart. Milestones
  // routinely land days apart (all five of cox's did), and placing them on a
  // real date axis stacks them into one illegible clump — so each card carries
  // its own delivery date instead. Don't reintroduce gridlines here without
  // solving that collision first.
  const fmt=t=>new Date(t).toLocaleDateString([],{month:"short",day:"numeric",year:"numeric"});
  const quarter=t=>{const d=new Date(t);return "Q"+(Math.floor(d.getMonth()/3)+1)+" "+d.getFullYear();};
  const n=rows.length;
  const cols=rows.map((r,i)=>{
    const prevNum=i?vnum(rows[i-1].ver):0,thisNum=vnum(r.ver);
    let prog=r.st==="reached"?100:(r.st==="planned"?0:Math.max(4,Math.min(96,Math.round((cur-prevNum)/Math.max(1,thisNum-prevNum)*100))));
    const col=r.st==="reached"?"var(--green)":(r.st==="active"?"var(--accent2)":"var(--muted)");
    const icon=r.st==="reached"?"circle-check-filled":(r.st==="active"?"progress":"flag");
    const tag=r.st==="reached"?'<span class="msk-tag reached">reached</span>':(r.st==="active"?'<span class="msk-tag active">in progress</span>':'<span class="msk-tag">planned</span>');
    const lineDone=r.st==="reached";const prevDone=i>0&&rows[i-1].st==="reached";
    return `<div class="mscol">
      <div class="msrail"><div class="msline ${i===0?'hide':''} ${prevDone?'done':''}"></div>
        <div class="msknob ${r.st}" style="--kc:${col}"><i class="ti ti-${icon}"></i></div>
        <div class="msline ${i===n-1?'hide':''} ${lineDone?'done':''}"></div></div>
      <div class="mscard ${r.st}">
        <div class="mstitle" title="${esc(r.name)}">${esc(r.name)}</div>
        <div class="msmetaline"><span class="msver">v${esc(r.ver)}</span> ${tag}</div>
        <div class="msq"><i class="ti ti-calendar-event" style="font-size:12px"></i> ${quarter(r.end)} · ${fmt(r.end)}</div>
        <div class="msprog"><div style="width:${prog}%;background:${col}"></div></div>
        <div class="msgoaltxt" title="${esc(r.goal)}">${esc(r.goal)}</div></div></div>`;}).join("");
  return `<div class="sec" style="margin-top:22px">Milestone roadmap <span style="font-size:11px;color:var(--dim);font-weight:400">· ${esc(proj)} — shippable targets in delivery order, each with the date it lands</span></div>
    <div class="panel" style="overflow-x:auto;margin-bottom:26px" data-keepscroll="rm-milestones"><div class="mstepper">${cols}</div></div>`;
}
// Milestone roadmap — each is a shippable target (a version), spanning several
// sprints. Status derived from the shipped version.
function milestonesHtml(s){
  const ms=s.milestones||[];if(!ms.length)return "";
  const cur=String(s.current_version||"0.0.0");const sprintsRun=(s.sprints||[]).length+(s.sprint?1:0);
  let activeShown=false;
  const rows=ms.map((m,i)=>{
    const reached=cmpVer(cur,m.target_version)>=0;
    const active=!reached&&!activeShown;if(active)activeShown=true;
    const col=reached?"var(--green)":(active?"var(--accent2)":"var(--muted)");
    const icon=reached?"circle-check-filled":(active?"target":"flag");
    const tag=reached?'<span class="pbadge" style="background:color-mix(in srgb,var(--green) 18%,transparent);color:var(--green)">reached</span>':(active?'<span class="pbadge on">in progress</span>':'<span class="pbadge off">planned</span>');
    // "reached" is derived from the version; goal_complete is the human/PO
    // call the release pipeline actually waits on. Offer the one click here
    // instead of leaving the PO daily to flag the same drift forever.
    const doneBtn=(reached&&!m.goal_complete)?` <button class="gc-btn" style="font-size:11px;padding:2px 8px" data-m="${escAttr(m.name)}" onclick="milestoneComplete(this.dataset.m)"><i class="ti ti-check"></i> Mark complete</button>`:(m.goal_complete?' <span class="pbadge" style="color:var(--green)">✓ complete</span>':'');
    return `<div class="msrow">
      ${i<ms.length-1?'<div class="msline-c"></div>':''}
      <div class="msdot" style="color:${col};border-color:${col}"><i class="ti ti-${icon}"></i></div>
      <div class="msmeta"><div class="msname">${esc(m.name)} <span class="msver">v${esc(m.target_version)}</span> ${tag}${doneBtn}</div>
        <div class="msgoal">${esc(m.goal)}</div></div></div>`;}).join("");
  return `<div class="sec" style="margin-top:4px">Milestones <span style="font-size:11px;color:var(--dim);font-weight:400">· shippable targets — each spans several sprints (${sprintsRun} run so far)</span></div>
    <div class="panel msline">${rows}</div>`;
}

// One click on a reached-but-unconfirmed milestone: the explicit completion
// the release pipeline waits for.
async function milestoneComplete(name){
  try{
    const r=await fetch(api("/milestone-complete/"+encodeURIComponent(name)),{method:"POST"});
    if(!r.ok){toast("Could not mark complete: "+(await r.text()));return;}
    toast("Milestone '"+name+"' marked complete");
  }catch(e){toast("Could not mark complete");}
}
function designSystemHtml(ds){
  if(!ds)return "";
  const has=(ds.principles||(ds.palette||[]).length||ds.typography||(ds.components||[]).length);
  if(!has)return "";
  const swatch=t=>{const m=(t||"").match(/#([0-9a-fA-F]{3,8})/);const c=m?m[0]:"var(--card2)";
    return `<div style="display:flex;align-items:center;gap:7px;background:var(--card2);border:1px solid var(--border2);border-radius:8px;padding:5px 9px;font-size:12px">
      <span style="width:14px;height:14px;border-radius:4px;background:${c};border:1px solid var(--border);flex-shrink:0"></span>${esc(t)}</div>`;};
  const chips=(arr,ic)=>((arr||[]).map(x=>`<span class="fchip" style="cursor:default"><i class="ti ti-${ic}" style="font-size:13px"></i> ${esc(x)}</span>`).join(""));
  return `<div class="panel" style="margin-top:16px">
    <h4 style="margin-top:0"><i class="ti ti-palette" style="color:var(--accent)"></i> Design system <span style="font-size:11px;color:var(--dim);font-weight:400">· authored by PD</span></h4>
    ${ds.principles?`<div style="font-size:13px;color:var(--muted);margin-bottom:10px">${esc(ds.principles)}</div>`:""}
    ${(ds.palette||[]).length?`<div style="display:flex;flex-wrap:wrap;gap:8px;margin-bottom:10px">${ds.palette.map(swatch).join("")}</div>`:""}
    ${ds.typography?`<div style="display:flex;align-items:center;gap:7px;font-size:12px;color:var(--muted);margin-bottom:10px"><i class="ti ti-typography" style="color:var(--accent2)"></i> ${esc(ds.typography)}</div>`:""}
    ${(ds.components||[]).length?`<div style="display:flex;flex-wrap:wrap;gap:6px">${chips(ds.components,"components")}</div>`:""}
  </div>`;
}
function roadmapCard(t){const pc={high:"var(--red)",medium:"var(--amber)",low:"var(--dim)"}[t.priority]||"var(--dim)";
  const deps=(t.depends_on||[]);
  const depBadge=deps.length?`<span class="b" style="background:color-mix(in srgb,var(--purple) 13%,transparent);color:var(--purple)"><i class="ti ti-link" style="font-size:11px"></i> ${deps.length}</span>`:"";
  const ui=t.has_ui?'<span class="b ui">UI</span>':'',bug=t.type==="bug"?'<span class="b bug">bug</span>':'';
  return `<div class="card-t" onclick="showTicket('${t.id}')" style="border-left:3px solid ${pc}">
    <div class="cid">${esc(t.id)}</div><div class="ct">${esc(t.title)}</div>
    <div class="badges"><span class="b ${t.priority}">${t.priority}</span>${ui}${bug}${depBadge}</div></div>`;}
function renderRoadmap(){
  const s=STATE,ts=s.tickets||[],hist=s.history||[];
  const el=document.getElementById("roadmap-body");if(!el)return;
  if(!ts.length){el.innerHTML='<div class="panel"><div class="empty">No tickets yet — the roadmap builds itself as work lands.</div></div>';return;}
  // Every status maps to exactly one bucket: fixed/verified/on_hold used to
  // match NOTHING, so those tickets vanished from the roadmap entirely and
  // the header math contradicted the columns ("0 of 5" over 3 visible cards).
  const isDone=t=>t.status==="done"||t.status==="documented"||t.status==="verified";
  const shipped=ts.filter(isDone);
  const inflight=ts.filter(t=>t.status==="in_progress"||t.status==="ready"||t.status==="fixed"||(t.type==="bug"&&t.status==="open"));
  const next=ts.filter(t=>t.status==="pending"&&(t.design&&t.design.technical));
  const later=ts.filter(t=>(t.status==="pending"&&!(t.design&&t.design.technical))||t.status==="on_hold");
  const total=ts.length,donePct=Math.round(shipped.length/total*100);
  const rank={high:0,medium:1,low:2};
  const sort=a=>a.slice().sort((x,y)=>(rank[x.priority]??3)-(rank[y.priority]??3));
  const prio=arr=>({high:arr.filter(t=>t.priority==="high").length,medium:arr.filter(t=>t.priority==="medium").length,low:arr.filter(t=>t.priority==="low").length});
  const sp=s.sprint;
  // Executive summary: headline progress + KPI tiles.
  const header=`<div class="panel" style="margin-bottom:16px"><div style="display:flex;align-items:center;gap:24px;flex-wrap:wrap">
    <div style="flex:1;min-width:220px">
      <div style="display:flex;justify-content:space-between;align-items:baseline;margin-bottom:8px">
        <div style="font-size:14px;font-weight:700;letter-spacing:.2px">Delivery progress</div>
        <div style="font-size:24px;font-weight:800;color:var(--accent2)">${donePct}%</div></div>
      <div style="height:10px;background:var(--card2);border-radius:6px;overflow:hidden">
        <div style="width:${donePct}%;height:100%;background:linear-gradient(90deg,var(--accent),var(--accent2))"></div></div>
      <div style="font-size:12px;color:var(--muted);margin-top:8px">${shipped.length} of ${total} shipped${sp?` &nbsp;·&nbsp; <i class="ti ti-flag" style="font-size:12px;color:var(--accent2)"></i> Sprint #${sp.number}: ${esc(sp.goal||'')}`:''}</div>
    </div>
    ${[["Shipped",shipped.length,"--green"],["In flight",inflight.length,"--amber"],["Planned",next.length+later.length,"--accent2"]].map(([k,v,c])=>`
      <div style="text-align:center;min-width:64px"><div style="font-size:28px;font-weight:800;color:var(${c});line-height:1">${v}</div><div style="font-size:10px;color:var(--muted);text-transform:uppercase;letter-spacing:.6px;margin-top:4px">${k}</div></div>`).join("")}
  </div></div>`;
  // Phase board with per-phase priority breakdown.
  const phases=[
    ["Now","being built","--accent",sort(inflight)],
    ["Next","designed · queued","--accent2",sort(next)],
    ["Later","backlog","--muted",sort(later)],
    ["Shipped","delivered","--green",shipped.slice().reverse()],
  ];
  const board=`<div class="cols">${phases.map(([name,sub,col,items])=>{const p=prio(items);
    const chips=[p.high?`<span class="b high">${p.high} high</span>`:'',p.medium?`<span class="b medium">${p.medium} med</span>`:'',p.low?`<span class="b low">${p.low} low</span>`:''].filter(Boolean).join("");
    return `<div class="col"><h3><span class="dot" style="background:var(${col})"></span>${name}<span class="n">${items.length}</span></h3>
      <div style="font-size:11px;color:var(--dim);margin:-4px 0 6px">${sub}</div>
      ${chips?`<div style="display:flex;gap:4px;flex-wrap:wrap;margin-bottom:8px">${chips}</div>`:''}
      ${items.length?items.map(roadmapCard).join(""):'<div class="empty">—</div>'}</div>`;}).join("")}</div>`;
  // Delivery timeline from release history.
  let timeline='';
  if(hist.length){const recent=hist.slice(-8);
    timeline=`<div class="sec" style="margin-top:22px">Delivery timeline</div><div class="panel" style="overflow-x:auto" data-keepscroll="rm-timeline" data-scrollend="1">
      <div style="display:flex;align-items:flex-start;min-width:min-content;padding:6px 0">${recent.map((r,i)=>`
        <div style="flex:1 1 0;min-width:118px;max-width:190px;position:relative;text-align:center;padding:0 4px">
          ${i<recent.length-1?'<div style="position:absolute;top:8px;left:50%;width:100%;height:2px;background:var(--border2);pointer-events:none"></div>':''}
          <div style="width:16px;height:16px;border-radius:50%;background:transparent;border:3px solid var(--green);margin:0 auto;position:relative;z-index:1"></div>
          <div style="font-size:13px;font-weight:700;margin-top:7px;color:var(--accent2)">v${esc(r.version)}</div>
          <div style="font-size:11px;color:var(--muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="${esc(r.title)}">${esc(r.title)}</div>
          <div style="font-size:10px;color:var(--dim)">${esc((r.at||'').slice(0,10))}</div></div>`).join("")}</div></div>`;
  }
  const keep=grabScroll(el);
  el.innerHTML=header+timeline+roadmapGantt(s)+board;
  applyScroll(el,keep);
}
// A view re-renders on every state poll, and replacing innerHTML resets each
// scroll container to its left edge — which yanks the reader back to the start
// of the timeline mid-scroll. Snapshot by key and put it back. A container
// marked data-scrollend opens on its newest (right-most) entry.
function grabScroll(root){const m={};
  root.querySelectorAll("[data-keepscroll]").forEach(e=>{
    m[e.dataset.keepscroll]={left:e.scrollLeft,atEnd:e.scrollLeft>=e.scrollWidth-e.clientWidth-4};});
  return m;}
function applyScroll(root,m){
  root.querySelectorAll("[data-keepscroll]").forEach(e=>{
    const prev=m[e.dataset.keepscroll],end=e.dataset.scrollend==="1";
    // First render of an end-anchored strip opens on the newest entry; someone
    // already parked at the end follows new releases instead of drifting back.
    if(!prev)e.scrollLeft=end?e.scrollWidth:0;
    else if(prev.atEnd&&end)e.scrollLeft=e.scrollWidth;
    else e.scrollLeft=prev.left;
  });}
// A Gantt-style delivery chart on a real time axis: each shipped version is a
// bar spanning the time it took to ship (previous release → its own release),
// and the active sprint is a projected in-flight bar toward its goal.
function ganttHtml(s){
  const hist=(s.history||[]).map(r=>({...r,t:Date.parse(r.at)})).filter(r=>!isNaN(r.t)).sort((a,b)=>a.t-b.t);
  const sp=s.sprint;
  if(!hist.length&&!sp)return '';
  const now=Date.now();
  // Build rows: one per version (bar prev→this), plus a projected sprint bar.
  const rows=[];
  let prev=hist.length?hist[0].t-Math.max(60000,(hist.length>1?hist[1].t-hist[0].t:3600000)):now;
  hist.forEach(r=>{rows.push({label:'v'+r.version,sub:r.title,start:prev,end:r.t,kind:'shipped',ticket:r.ticket});prev=r.t;});
  if(sp){const spStart=hist.length?hist[hist.length-1].t:now-3600000;
    const projEnd=now+Math.max(1800000,(rows.length?(rows.reduce((a,r)=>a+(r.end-r.start),0)/rows.length):3600000));
    rows.push({label:'Sprint #'+sp.number,sub:sp.goal||'in progress',start:spStart,end:projEnd,kind:'active'});}
  if(!rows.length)return '';
  const min=Math.min(...rows.map(r=>r.start));
  const max=Math.max(now,...rows.map(r=>r.end));
  const span=Math.max(1,max-min);
  const pct=t=>((t-min)/span*100);
  // Axis ticks: 5 evenly spaced timestamps.
  const fmt=t=>{const d=new Date(t);const sameDay=(max-min)<86400000;
    return sameDay?d.toLocaleTimeString([],{hour:'2-digit',minute:'2-digit'}):d.toLocaleDateString([],{month:'short',day:'numeric'});};
  const ticks=[0,.25,.5,.75,1].map(f=>`<span style="position:absolute;left:${f*100}%;transform:translateX(-50%);font-size:10px;color:var(--dim)">${fmt(min+f*span)}</span>`).join("");
  const nowPct=pct(now);
  const bars=rows.map(r=>{const l=pct(r.start),w=Math.max(1.5,pct(r.end)-pct(r.start));
    const col=r.kind==='active'?'var(--amber)':'var(--green)';
    const stripe=r.kind==='active'?`background:repeating-linear-gradient(45deg,${col},${col} 6px,transparent 6px,transparent 12px);border:1px solid ${col}`:`background:${col}`;
    return `<div class="gantt-row">
      <div class="gantt-lbl" title="${esc(r.sub||'')}"><b>${esc(r.label)}</b><span>${esc((r.sub||'').slice(0,42))}</span></div>
      <div class="gantt-track"><div class="gantt-bar" style="left:${l}%;width:${w}%;${stripe}" title="${esc(r.label)} · ${esc(r.sub||'')}">${r.kind==='shipped'?'<i class="ti ti-check" style="font-size:11px;color:#fff"></i>':''}</div></div></div>`;}).join("");
  return `<div class="sec" style="margin-top:22px">Release Gantt <span style="font-size:11px;color:var(--dim);font-weight:400">· when each version shipped, and the sprint in flight</span></div>
    <div class="panel" style="overflow-x:auto" data-keepscroll="rm-gantt"><div style="min-width:520px">
      <div class="gantt-axis"><div class="gantt-lbl"></div><div class="gantt-track" style="height:16px">${ticks}<div style="position:absolute;left:${nowPct}%;top:-2px;bottom:-999px;width:2px;background:var(--accent2);opacity:.5" title="now"></div></div></div>
      ${bars}
      <div class="gantt-axis" style="margin-top:6px"><div class="gantt-lbl"></div><div class="gantt-track" style="height:0"><div style="position:absolute;left:${nowPct}%;transform:translateX(-50%);font-size:9px;color:var(--accent2)">now</div></div></div>
    </div></div>`;
}
function actIcon(action){const s=(action||"").toLowerCase();
  if(/propos/.test(s))return"bulb";if(/design|readied|ux/.test(s))return"ruler-2";
  if(/implement|built|feature/.test(s))return"code";if(/fix/.test(s))return"bug";
  if(/document/.test(s))return"book-2";if(/merg/.test(s))return"git-merge";
  if(/commit|push|branch|pr |pull request|opened pr/.test(s))return"git-commit";
  if(/reject|duplicate/.test(s))return"ban";if(/ship|deploy/.test(s))return"rocket";
  if(/review/.test(s))return"eye-check";if(/verif|test/.test(s))return"checkup-list";
  if(/priorit|edit/.test(s))return"adjustments";if(/sprint|standup|retro/.test(s))return"users-group";
  return"point";}
function relTime(at){if(!at)return"";const d=new Date(at.replace(" ","T"));const s=(Date.now()-d.getTime())/1000;
  if(s<60)return"just now";if(s<3600)return Math.floor(s/60)+"m ago";if(s<86400)return Math.floor(s/3600)+"h ago";return Math.floor(s/86400)+"d ago";}
// The per-cycle scorecard table: newest first, each cycle graded A–D by the
// backend (deterministic, zero tokens). The grade colors match intuition:
// A shipped, B useful, C idle-but-clean, D churn/incident.
function renderCycleScores(s){
  const el=document.getElementById("cycle-scores");if(!el)return;
  const rows=(s.cycle_scores||[]).slice(-12).reverse();
  if(!rows.length){el.innerHTML='<div class="empty">no cycles scored yet</div>';return;}
  const gc={A:"var(--green)",B:"var(--accent2)",C:"var(--muted)",D:"var(--red)"};
  el.innerHTML='<table class="scoretbl"><thead><tr><th></th><th>cycle</th><th>shipped</th><th>useful/runs</th><th>cost</th><th>errors</th><th>when</th></tr></thead><tbody>'+
    rows.map(r=>{
      // Per-phase breakdown (secs + $) as a hover title — where the cycle went.
      const secs=r.phase_secs||{},cost=r.phase_cost||{};
      const keys=[...new Set([...Object.keys(secs),...Object.keys(cost)])];
      const brk=keys.map(k=>{
        const t=secs[k]?(secs[k]>=60?Math.round(secs[k]/60)+'m':secs[k]+'s'):'';
        const c=cost[k]?('$'+cost[k].toFixed(2)):'';
        return k+': '+[t,c].filter(Boolean).join(' · ');
      }).join('\n');
      return `<tr title="${esc(brk)}">
      <td><span class="grade" style="background:color-mix(in srgb,${gc[r.grade]||'var(--muted)'} 16%,transparent);color:${gc[r.grade]||'var(--muted)'}">${esc(r.grade)}</span></td>
      <td>#${r.cycle}</td><td>${r.shipped||0}</td><td>${r.useful||0}/${r.runs||0}</td>
      <td>${r.cost_usd?('$'+r.cost_usd.toFixed(2)):'—'}</td>
      <td>${(r.errors||0)+(r.incidents?(' · '+r.incidents+'⛔'):'')}</td>
      <td style="color:var(--dim)">${esc((r.at||'').slice(11,16))}</td></tr>`;}).join("")+'</tbody></table>'
    +costPerShip(s.cycle_scores||[]);
}
// 7-day FinOps digest from the scorecard: cost per role + the headline number
// "cost per shipped ticket" — the KPI the engine-per-role tuning aims at.
function costPerShip(scores){
  const cutoff=Date.now()-7*86400000;
  const rows=scores.filter(r=>r.at&&new Date(r.at).getTime()>=cutoff);
  if(!rows.length)return "";
  let shipped=0,total=0;const byRole={};
  for(const r of rows){
    shipped+=r.shipped||0;total+=r.cost_usd||0;
    for(const[k,v]of Object.entries(r.phase_cost||{}))byRole[k]=(byRole[k]||0)+v;
  }
  if(total<0.005)return "";
  const roles=Object.entries(byRole).sort((a,b)=>b[1]-a[1]).slice(0,6);
  const per=shipped?("$"+(total/shipped).toFixed(2)):"∞ (nothing shipped)";
  return `<div class="cps"><div class="cps-head">7 days · $${total.toFixed(2)} spent · ${shipped} shipped · <b>${per}/ship</b></div>
    <div class="cps-bars">${roles.map(([k,v])=>{
      const w=Math.max(4,Math.round(v/total*100));
      return `<div class="cps-row" title="$${v.toFixed(2)}"><span class="cps-lbl">${esc(k)}</span><div class="cps-bar" style="width:${w}%"></div><span class="cps-val">$${v.toFixed(2)}</span></div>`;
    }).join("")}</div></div>`;
}
function actItem(a){const col=cvar(AC[a.agent]||"--muted");
  return `<div class="tlrow"><div class="tl-node" style="--nc:${col}"><i class="ti ti-${actIcon(a.action)}"></i></div>
    <div class="tl-body"><div class="tl-line"><span class="tl-who" style="color:${col}">${esc(a.agent)}</span> <span class="tl-act">${esc(a.action)}</span>${a.ticket?` <span class="tk" onclick="showTicket('${esc(a.ticket)}')" style="cursor:pointer">${esc(a.ticket)}</span>`:''}</div>
      <div class="tl-t" title="${esc(a.at||'')}">${esc(relTime(a.at))}</div></div></div>`;}
function card(t){const a={high:"var(--red)",medium:"var(--amber)",low:"var(--dim)"}[t.priority]||"var(--dim)";
  const ui=t.has_ui?'<span class="b ui">UI</span>':'',bug=t.type==="bug"?'<span class="b bug">bug</span>':'';
  // A human-assigned ticket is out of the agent pool — say WHO owns it.
  const who=t.assignee?`<span class="b" style="background:var(--accentbg);color:var(--accent2)"><i class="ti ti-user" style="font-size:10px"></i> @${esc(t.assignee)}</span>`:'';
  const doneSet=["done","documented","verified","rejected"];
  const blockers=(t.depends_on||[]).filter(d=>{const dt=(STATE.tickets||[]).find(x=>x.id===d);return dt&&!doneSet.includes(dt.status);});
  const blocked=blockers.length?`<span class="b" style="background:color-mix(in srgb,var(--red) 16%,transparent);color:var(--red)" title="blocked by ${esc(blockers.join(', '))}"><i class="ti ti-lock" style="font-size:10px"></i> blocked</span>`:'';
  const hold=t.status==="on_hold"?`<span class="b" style="background:color-mix(in srgb,var(--amber) 18%,transparent);color:var(--amber)" title="${esc((STATE.hold_reasons||{})[t.id]||'on hold')}"><i class="ti ti-player-pause" style="font-size:10px"></i> on hold</span>`:'';
  return `<div class="card-t" onclick="showTicket('${t.id}')" ${t.status==="on_hold"?'style="opacity:.65"':''}><div class="cid">${esc(t.id)}</div>
    <div class="ct">${esc(t.title)}</div><div class="badges"><span class="b ${t.priority}">${t.priority}</span>${hold}${blocked}${ui}${bug}${who}</div></div>`;}
function column([k,l,c],ts){const items=ts.filter(t=>t.status===k);
  return `<div class="col"><h3><span class="dot" style="background:var(${c})"></span>${l}<span class="n">${items.length}</span></h3>${items.length?items.map(card).join(""):'<div class="empty">—</div>'}</div>`;}
// Unified column: collects both features and bugs whose status maps to this stage.
function ucolumn([k,l,c,statuses],ts){const items=ts.filter(t=>statuses.includes(t.status))
    .sort((a,b)=>(a.type==="bug")-(b.type==="bug")); // bugs after features in the same column
  return `<div class="col"><h3><span class="dot" style="background:var(${c})"></span>${l}<span class="n">${items.length}</span></h3>${items.length?items.map(card).join(""):'<div class="empty">—</div>'}</div>`;}

// Merged-then-reverted work (CXA-F047) on the Work board: the most recent
// events, with the approve/dismiss decision pending ones still wait on.
// Renders nothing when the ledger is empty — the board reads as before.
function renderRevertedWork(s){
  const el=document.getElementById("board-reverts");if(!el)return;
  const rv=s.reverted_work||[];
  if(!rv.length){el.innerHTML="";return;}
  const row=e=>`<div class="rel" style="align-items:center">
    <span class="rv" style="background:color-mix(in srgb,var(--red) 14%,transparent);color:var(--red)"><i class="ti ti-arrow-back-up"></i></span>
    <div class="rt">${esc(e.ticket)} — reverted work<div class="rd">${esc(e.subject)} · ${esc(e.role)} · ${e.decision==="pending"?"awaiting review":esc(e.decision)}</div></div>
    ${e.decision==="pending"?`<span class="ibx-acts" onclick="event.stopPropagation()">${ibtn("Dismiss",`inboxRevert('${esc(e.sha)}','dismiss')`)+ibtn("Confirm",`inboxRevert('${esc(e.sha)}','approve')`,1)}</span>`:""}</div>`;
  el.innerHTML=`<div class="panel" style="margin-bottom:12px"><h4><i class="ti ti-arrow-back-up" style="color:var(--red)"></i> Reverted work</h4>${[...rv].reverse().slice(0,5).map(row).join("")}</div>`;
}

// Engine-health detail for one agent role: what failed, how often, and when —
// the line on the card is the headline, this is the story.
function showRoleHealth(role){
  const hl=(STATE.role_health||{})[role];if(!hl)return;
  const when=hl.last_error_at?relTime(hl.last_error_at):"—";
  coxModal({title:role+" · engine health",
    message:`${hl.errors} error(s), ${hl.timeouts} timeout(s) recorded.\n\nMost recent (${when}):\n${hl.last_error||"—"}\n\nTimeouts mean the provider stalled — failover retried on the fallback chain. Frequent timeouts under parallel load usually mean the concurrency is too high for the provider; lower it in Settings → Workflow.`,
    confirmText:"OK",cancelText:"Open live log"}).then(ok=>{if(!ok)openAgent(role);});
}

function renderSidebar(s){
  document.getElementById("ver").textContent=s.current_version||"0.0.0";
  document.getElementById("pn-tickets").textContent=(s.tickets||[]).length+" tickets";
  document.title="CoXAgent · "+(document.getElementById("proj-name").textContent||"");}
function renderActive(){const s=STATE; if(!s.tickets&&!s.activity&&CUR==="overview")return;
  if(CUR==="overview"){
    renderDriftAlerts(s);
    if(!(s.tickets||[]).length&&!(s.activity||[]).length){
      document.getElementById("kpis").innerHTML=`<div class="panel" style="grid-column:1/-1;text-align:center;padding:40px 20px">
        <i class="ti ti-rocket" style="font-size:34px;color:var(--accent2)"></i>
        <div style="font-size:17px;font-weight:600;margin:12px 0 4px">Ready to build</div>
        <div style="color:var(--muted);font-size:13px;max-width:440px;margin:0 auto 6px">The team is standing by. Press <b style="color:var(--text)">Start</b> to run the loop — BA proposes features, SA designs them, DEV builds, TEST verifies, DOCS writes guides.</div>
        <div style="color:var(--dim);font-size:12px">Set engines and workflow in <a onclick="nav('settings')" style="color:var(--accent2);cursor:pointer">Settings</a>.</div></div>`;
      document.getElementById("ov-alerts").innerHTML='';
      document.getElementById("ov-deploy").innerHTML='';
      document.getElementById("ov-activity").innerHTML='<div class="empty">activity appears as agents work</div>';
      document.getElementById("ov-changelog").innerHTML='<div class="empty">no releases yet</div>';
      const gov=document.getElementById("ov-attention");if(gov)gov.innerHTML='';
      return;
    }
    const m=metricsFrom(s);
    const spend=s.spend||{};
    document.getElementById("ov-alerts").innerHTML=alertsHtml(s,m,spend);
    drainBanner("ov-drain");
    document.getElementById("kpis").innerHTML=[kpi("Shipped",m.shipped),kpi("In flight",m.inflight),kpi("Documented",m.docd),kpi("Releases",m.releases),kpi("Cost",money(spend.total_cost_usd))].join("");
    renderHealth(s);
    const dp=s.deploy;
    document.getElementById("ov-deploy").innerHTML=dp?`<div class="panel" style="margin-top:16px;display:flex;align-items:center;gap:13px">
      <div class="av" style="width:36px;height:36px;background:${dp.ok?'var(--green)':'var(--red)'}22;color:${dp.ok?'var(--green)':'var(--red)'}"><i class="ti ti-${dp.ok?'cloud-check':'cloud-x'}"></i></div>
      <div style="flex:1"><div style="font-size:13px;font-weight:600">Deployment ${dp.ok?'healthy':'failed'}</div><div style="font-size:12px;color:var(--muted)">${esc(dp.summary)}</div></div>
      <div style="font-size:11px;color:var(--dim)">${esc((dp.at||"").slice(0,16).replace("T"," "))}</div></div>`:"";
    document.getElementById("ov-charts").innerHTML=chartsHtml(s);
    loadGovernanceAttention();
    document.getElementById("ov-design").innerHTML=designSystemHtml(s.design_system);
    const act=[...(s.activity||[])].reverse().slice(0,7);
    document.getElementById("ov-activity").innerHTML=act.length?act.map(actItem).join(""):'<div class="empty">no activity yet</div>';
    const h=s.history||[];
    document.getElementById("ov-changelog").innerHTML=h.length?[...h].reverse().slice(0,6).map(r=>`<div class="rel"><span class="rv">v${esc(r.version)}</span><div class="rt">${esc(r.title)}<div class="rd">${esc((r.at||"").split("T")[0])} · ${esc(r.ticket)}</div></div></div>`).join(""):'<div class="empty">no releases yet</div>';
  }else if(CUR==="team"){
    const acts=s.activity||[],tickets=s.tickets||[],spend=s.spend||{by_role:{}};
    const inProg=tickets.filter(t=>t.status==="in_progress");
    // Per-agent tallies from the activity trail.
    const stat={};acts.forEach(a=>{const k=a.agent;(stat[k]=stat[k]||{n:0,tk:new Set(),last:null});stat[k].n++;if(a.ticket)stat[k].tk.add(a.ticket);stat[k].last=a;});
    const shipped=tickets.filter(t=>t.status==="done"||t.status==="documented").length;
    const totalActs=acts.length,totalCost=spend.total_cost_usd||0;
    document.getElementById("team-summary")&&(document.getElementById("team-summary").innerHTML=
      kpi("Agent actions",totalActs)+kpi("Tickets shipped",shipped)+kpi("Bugs open",tickets.filter(t=>t.type==="bug"&&t.status==="open").length)+kpi("Team cost",money(totalCost)));
    document.getElementById("agents").innerHTML=AGENTS.map(([r,d,c])=>{
      const col=cvar(c);const st=stat[r]||{n:0,tk:new Set(),last:null};
      const roleKey=r.toLowerCase().replace(/-/g,"_");
      const cost=(spend.by_role||{})[roleKey]||0;
      // The engine this role ACTUALLY ran on (post-failover) and the user whose
      // runner last ran it — both last-wins from the spend meter. Until a first
      // run has finished (the meter folds at run END — a long run would leave
      // the badge blank for an hour), fall back to the CONFIGURED engine for
      // the role: that is the engine being launched right now, short of a
      // failover, and the observed value replaces it as soon as one lands.
      const engCfg=(window._cfg&&_cfg.engine)?(((_cfg.engine.per_role||{})[roleKey]||{}).engine||( _cfg.engine.default||{}).engine||""):"";
      const eng=(spend.engine_by_role||{})[roleKey]||engCfg;
      const lastOp=(spend.operator_by_role||{})[roleKey]||"";
      // Live "working now" from the SHARED registry — EVERY team (this hub or
      // another machine) running this agent, so one card shows N users at once.
      const localActive=(window.RUNNER&&RUNNER.mode==="running")?(RUNNER.active_role||"").replace(/_/g,"-").toUpperCase():"";
      const runners=(window.WORKERS||[]).filter(w=>(w.role||"").replace(/_/g,"-").toUpperCase()===r)
        .map(w=>({who:w.worker,ticket:w.ticket||""}));
      // Ensure the local runner is present (its snapshot is freshest).
      if(localActive===r&&window.RUNNER){
        const me=(RUNNER.operator||'')+(RUNNER.host?'@'+RUNNER.host:'');
        const mine=runners.find(x=>x.who===me);
        if(mine){mine.ticket=RUNNER.active_note||mine.ticket;} else {runners.unshift({who:me,ticket:RUNNER.active_note||''});}
      }
      const working=runners.length>0;
      // Fallback ticket for the idle "last touched" line.
      let cur=null,live=working;
      if(r==="DEV-FEATURE"||r==="DEV-BUG"){const w=inProg.find(t=>r==="DEV-BUG"?t.type==="bug":t.type!=="bug");if(w){cur=w.id;}}
      if(!cur&&st.last&&st.last.ticket)cur=st.last.ticket;
      // Name the machine only when this account runs on more than one.
      const allWho=runners.map(x=>x.who);
      const short=w=>workerLabel(w,allWho);
      const taskChip=(note,tid,cls)=>`<span class="ag-task ${cls}"${tid?` onclick="event.stopPropagation();showTicket('${tid}')" style="cursor:pointer"`:''}>${note?esc(note):''}${tid?`<span class="tid">${esc(tid)}</span>`:''}</span>`;
      let statusHtml;
      if(working){
        const rows=runners.map(rn=>`<div class="ag-run" onclick="event.stopPropagation();openAgent('${r}','${esc(rn.who)}')" style="cursor:pointer" title="View ${esc(short(rn.who))}'s live log">`
          +`<span class="ag-task">${rn.ticket?`<span class="tid">${esc(rn.ticket)}</span>`:'working'}</span>`
          +`<span class="ag-by" title="${esc(rn.who)}"><i class="ti ti-user-cog"></i> ${esc(short(rn.who))}</span></div>`).join("");
        statusHtml=`<div class="ag-now"><i class="ti ti-loader-2 att-spin"></i> working now${runners.length>1?`<span class="ag-nteams">${runners.length} teams</span>`:''}</div>
          <div class="ag-runs">${rows}</div>`;
      }else{
        // Say WHY it idles, not just that it does — the difference between
        // "nothing scoped for DEV" and "engine down" is the whole diagnosis.
        let idleWhy='';
        if(!cur){
          const scoped=((s.sprint&&s.sprint.committed)||[]).map(id=>((s.tickets||[]).find(x=>x.id===id)||{}));
          if(r.startsWith('DEV')&&!scoped.some(t=>["ready","open"].includes(t.status)))idleWhy=' — no scoped work';
          else if(s.ops_down)idleWhy=' — ops down';
          else idleWhy=' — waiting for its phase';
        }
        statusHtml=`<div class="ag-last"><i class="ti ti-point"></i> ${cur?'last touched':'idle'+idleWhy}${lastOp?` · <span class="ag-by" title="${esc(lastOp)}"><i class="ti ti-user-cog"></i> ${esc(workerLabel(lastOp,[lastOp]))}</span>`:''}</div>
          ${cur?taskChip('',cur,'idle'):''}`;
      }
      // Which engine CLI this role is really on — copilot/opencode/claude/… —
      // stamped from the run that actually happened, so failover shows through.
      const engBadge=eng?`<span class="ag-eng" title="engine actually running this agent">${esc(eng)}</span>`:'';
      const hl=(s.role_health||{})[r];
      const healthHtml=hl&&hl.errors>0?`<div class="ag-health" title="click for details" onclick="event.stopPropagation();showRoleHealth('${esc(r)}')"><i class="ti ti-alert-triangle"></i> ${hl.errors} error${hl.errors===1?'':'s'}${hl.timeouts?` · ${hl.timeouts} timeout${hl.timeouts===1?'':'s'}`:''}${hl.timeouts>=3?' · <b>provider under load — consider a lower concurrency</b>':''}</div>`:'';
      return `<div class="agent ${live?'run':''}" onclick="openAgent('${r}')" style="cursor:pointer">
        <div class="ag-head"><div class="av" style="background:${col}22;color:${col}">${initials(r)}<span class="sr"></span></div>
          <div class="ag-id"><div class="rl">${r}${engBadge}</div><div class="ds">${d}</div></div>
          <i class="ti ti-terminal-2 ag-term"></i></div>
        <div class="agstats"><span title="actions"><i class="ti ti-bolt"></i> ${st.n}</span><span title="tickets touched"><i class="ti ti-ticket"></i> ${st.tk.size}</span>${cost>0?`<span title="cost"><i class="ti ti-coin"></i> ${money(cost)}</span>`:''}</div>
        <div class="ag-status">${statusHtml}</div>${healthHtml}</div>`;}).join("");
    renderDupWarn(s);
    renderCycleScores(s);
    renderTeamsOnline();
    renderSessions();
    // Populate whenever the card is VISIBLE — applyRole shows .admin-only for
    // the hub-admin tier (admin, super, open mode; see isHubAdmin). Gating on
    // role==="admin" alone stranded this card on "loading…" twice: open mode
    // once, and the hub owner's "super" role until CXA-B132.
    if(isHubAdmin())renderTeamPeople();
  }else if(CUR==="board"){
    let feats=(s.tickets||[]).filter(t=>t.type!=="bug"),bugs=(s.tickets||[]).filter(t=>t.type==="bug");
    if(BF!=="all"){feats=feats.filter(t=>t.priority===BF);bugs=bugs.filter(t=>t.priority===BF);}
    const fc=["all","high","medium","low"];
    const sc=["all","pending","ready","in_progress","open","fixed","done","documented","verified","on_hold","rejected"];
    document.getElementById("board-filters").innerHTML=
      fc.map(f=>`<span class="fchip ${BF===f?'on':''}" onclick="BF='${f}';renderActive()">${f==='all'?'all priorities':f}</span>`).join("")
      +'<span style="width:1px;background:var(--border2);margin:0 4px;align-self:stretch"></span>'
      +sc.map(f=>`<span class="fchip ${SF===f?'on':''}" onclick="SF='${f}';renderActive()">${f==='all'?'all statuses':f.replace('_',' ')}</span>`).join("");
    // One unified board: features + bugs share columns mapped by lifecycle stage.
    let all=(s.tickets||[]);if(BF!=="all")all=all.filter(t=>t.priority===BF);
    if(SF!=="all")all=all.filter(t=>t.status===SF);
    document.getElementById("board-cols").innerHTML=UCOLS.map(c=>ucolumn(c,all)).join("");
    renderRevertedWork(s);
    renderSprintPanel(s);
    renderBacklogPanel(s);
  }else if(CUR==="activity"){
    const sel=document.getElementById("act-filter");
    if(!INIT_ACT){const agents=[...new Set((s.activity||[]).map(a=>a.agent))];
      sel.innerHTML='<option value="">all agents</option>'+agents.map(a=>`<option>${esc(a)}</option>`).join("");INIT_ACT=true;}
    const f=sel.value;let act=[...(s.activity||[])].reverse();if(f)act=act.filter(a=>a.agent===f);
    let html="",lastDay="";
    for(const a of act){const day=(a.at||"").slice(0,10);
      if(day&&day!==lastDay){lastDay=day;html+=`<div class="tl-day">${esc(day)}</div>`;}
      html+=actItem(a);}
    document.getElementById("activity-full").innerHTML=act.length?`<div class="timeline">${html}</div>`:'<div class="empty">no activity yet</div>';
    if(typeof renderAlerts==="function")renderAlerts();
    renderTranscripts();
    // CXA-B131: the Work log panel must never sit on 'loading…' when the
    // agent drawer was never opened — paint its terminal state here.
    if(typeof paintAgentLogIdle==="function")paintAgentLogIdle();
  }else if(CUR==="insights"){
    const sp=s.spend||{by_role:{}};const tok=(sp.input_tokens||0)+(sp.output_tokens||0);
    // These KPIs are the real measured totals — no counterfactual. The old
    // "không nén ~$X" subtitle scaled a 200-sample char saving against the
    // lifetime token total: mismatched units and scope, so it read as ~1.4%
    // and looked fabricated. The token-saver's true, honest ratio lives in its
    // own panel below (78% off the output it actually compressed).
    const drawKpis=()=>{
      setHTML(document.getElementById("cost-kpis"),
        [kpi("Total spend",money(sp.total_cost_usd)),kpi("Tokens",fmtK(tok)),kpi("Runs",sp.runs||0)].join(""));
    };
    drawKpis();
    if(!window._tsCacheAt||Date.now()-window._tsCacheAt>60000){
      window._tsCacheAt=Date.now();
      fetch("/api/token-saver").then(r=>r.json()).then(ts=>{window._tsCache=ts;if(CUR==="insights")renderTokenSaver();}).catch(()=>{});
    }
    const roles=Object.entries(sp.by_role||{}).sort((a,b)=>b[1]-a[1]);
    const max=roles.length?roles[0][1]:1;
    setHTML(document.getElementById("cost-roles"),roles.length?roles.map(([r,c])=>{
      const col=cvar(AC[r.toUpperCase().replace("_","-")]||"--accent2");const pct=Math.max(3,Math.round(c/max*100));
      return `<div style="display:flex;align-items:center;gap:12px;padding:8px 0"><span style="min-width:110px;font-size:12px">${esc(r)}</span>
        <div style="flex:1;background:var(--card2);border-radius:6px;height:8px;overflow:hidden"><div style="width:${pct}%;height:100%;background:${col}"></div></div>
        <span style="font-size:12px;font-family:ui-monospace,monospace;min-width:58px;text-align:right">${money(c)}</span></div>`;}).join(""):'<div class="empty">no spend yet — runs on scripted/mock cost $0</div>');
    const ops=Object.entries(sp.by_operator||{}).map(([o,v])=>[o,v||{}]).sort((a,b)=>(b[1].cost_usd||0)-(a[1].cost_usd||0));
    const omax=ops.length?(ops[0][1].cost_usd||0):1;
    setHTML(document.getElementById("cost-operators"),ops.length?ops.map(([o,v])=>{
      const c=v.cost_usd||0;const tk=(v.input_tokens||0)+(v.output_tokens||0);const pct=Math.max(3,Math.round(c/(omax||1)*100));
      return `<div style="display:flex;align-items:center;gap:12px;padding:8px 0"><span style="min-width:150px;font-size:12px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="${esc(o)}">${esc(o)}</span>
        <div style="flex:1;background:var(--card2);border-radius:6px;height:8px;overflow:hidden"><div style="width:${pct}%;height:100%;background:var(--accent)"></div></div>
        <span style="font-size:11px;color:var(--muted);min-width:64px;text-align:right">${fmtK(tk)} tok</span>
        <span style="font-size:12px;font-family:ui-monospace,monospace;min-width:58px;text-align:right">${money(c)}</span></div>`;}).join(""):'<div class="empty">no per-user spend yet — starts recording once an operator runs a cycle</div>');
    const cap=(window._budget!==undefined)?window._budget:null;
    if(cap&&cap>0){const used=Math.min(100,Math.round((sp.total_cost_usd||0)/cap*100));
      setHTML(document.getElementById("cost-budget"),`<div style="display:flex;justify-content:space-between;font-size:12px;margin-bottom:8px"><span>${money(sp.total_cost_usd)} of ${money(cap)}</span><span style="color:var(--muted)">${used}%</span></div>
        <div style="background:var(--card2);border-radius:6px;height:10px;overflow:hidden"><div style="width:${used}%;height:100%;background:${used>=90?'var(--red)':used>=70?'var(--amber)':'var(--accent2)'}"></div></div>
        <div style="color:var(--dim);font-size:11px;margin-top:8px">Loop auto-pauses when the cap is reached.</div>`);
    }else{setHTML(document.getElementById("cost-budget"),'<div class="empty">no budget cap set — add "budget_usd" in coxagent.json</div>');}
    // Engine reliability: the question "is GLM healthy today?" answered
    // where cost already lives, instead of only in hub.log greps.
    const rh=Object.entries(s.role_health||{}).sort((a,b)=>((b[1].errors||0)+(b[1].timeouts||0))-((a[1].errors||0)+(a[1].timeouts||0)));
    setHTML(document.getElementById("cost-engines"),rh.length?rh.map(([r,h])=>{
      const total=(h.errors||0)+(h.timeouts||0);
      const today=(h.last_error_at||"").slice(0,10)===new Date().toISOString().slice(0,10);
      const col=today?"var(--red)":(total?"var(--amber)":"var(--green)");
      return `<div style="display:flex;align-items:center;gap:12px;padding:7px 0">
        <span style="min-width:110px;font-size:12px">${esc(r)}</span>
        <span style="font-size:12px;color:${col};min-width:150px">${h.errors||0} error(s) &middot; ${h.timeouts||0} timeout(s)</span>
        <span style="flex:1;font-size:11px;color:var(--muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="${escAttr(h.last_error||"")}">${today?"today: ":""}${esc((h.last_error||"").slice(0,90))}</span></div>`;
    }).join(""):'<div class="empty">no engine failures recorded — all roles healthy</div>');
    renderTokenSaver();
  }else if(CUR==="discuss"){renderDiscuss();}else if(CUR==="roadmap"){renderRoadmap();}else if(CUR==="home"){renderHome();}else if(CUR==="mg-spaces"){renderManage();}else if(CUR==="mg-users"){renderManage();}else if(CUR==="mg-usage"){renderManage();}else if(CUR==="mg-audit"){renderManage();}}
