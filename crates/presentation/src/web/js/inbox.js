// Inbox: the hybrid-team "waiting for me" queue — tickets pending my
// approval, evidence awaiting my verdict, exception tickets routed to me,
// questions addressed to me, and PRs held for human eyes.
// Split from index.html — classic script, one shared scope (see core.js).

async function loadInbox(){
  try{return await(await fetch(api("/inbox"))).json();}catch(e){return {items:[]};}
}

function inboxBadge(n){
  const b=document.getElementById("inbox-badge");if(!b)return;
  b.textContent=n;b.style.display=n>0?"":"none";
}

const INBOX_KIND={
  approve_ready:{label:"Approve to Ready",ic:"ti-checks",col:"var(--accent2)"},
  verify:{label:"Verify fix",ic:"ti-shield-check",col:"var(--green)"},
  assigned:{label:"Assigned to you",ic:"ti-user",col:"var(--purple)"},
  question:{label:"Question for you",ic:"ti-help-circle",col:"var(--amber)"},
  review_pr:{label:"PR held for human",ic:"ti-git-pull-request",col:"var(--teal)"},
  auto_approved:{label:"Auto-approved",ic:"ti-robot",col:"var(--dim)"},
};

function inboxCard(kind,meta,title,actions,ticket){
  const k=INBOX_KIND[kind]||{label:kind,ic:"ti-inbox",col:"var(--muted)"};
  const open=ticket?`onclick="showTicket('${esc(ticket)}')"`:"";
  return `<div class="panel" ${open} style="margin-bottom:12px;display:flex;gap:14px;align-items:center;${ticket?'cursor:pointer;':''}transition:border-color .15s" onmouseover="this.style.borderColor='${k.col}'" onmouseout="this.style.borderColor='var(--border)'">
    <div style="width:38px;height:38px;border-radius:10px;background:color-mix(in srgb,${k.col} 14%,transparent);display:flex;align-items:center;justify-content:center;flex-shrink:0">
      <i class="ti ${k.ic}" style="font-size:18px;color:${k.col}"></i></div>
    <div style="min-width:0;flex:1">
      <div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap">
        <span style="font-size:11px;font-weight:700;letter-spacing:.4px;text-transform:uppercase;color:${k.col}">${k.label}</span>
        <span style="font-size:11.5px;color:var(--dim)">${meta}</span></div>
      <div style="font-size:13.5px;font-weight:600;margin-top:3px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${title}</div></div>
    <div class="ibx-acts" onclick="event.stopPropagation()">${actions}</div></div>`;
}
// The primary action is rendered LAST and pinned right, whatever else a card
// carries. Cards used to lay their buttons out in reading order, so the one
// cyan block landed in the middle of a 3-button row and on the end of a
// 2-button row — down a list it zig-zagged, and the eye tracked the colour
// instead of the text.
const ibtn=(label,fn,pri)=>`<button class="tk-btn${pri?' go':''} ibx-btn${pri?' ibx-pri':''}" onclick="${fn}">${label}</button>`;

// Who may take which gate decision — the same rule the server enforces
// (AuthRole::can_approve_ready / can_verify). Mirrored here only to keep a
// button off screen when it would 403; the server is the authority.
const LEADS=["super","admin","director","manager","techlead","dslead","dalead"];
const myRole=()=>((window.ME&&ME.role)||"").toLowerCase();
// No accounts configured = open mode: the one operator decides everything.
const canApproveReady=()=>!window.ME||!ME.role||LEADS.includes(myRole())||myRole()==="ba";
const canVerify=()=>!window.ME||!ME.role||LEADS.includes(myRole())||myRole()==="reviewer";
// A gate the viewer cannot act on still shows WHY, rather than a bare row.
const noRight=()=>'<span class="ibx-noright" title="Your role may not take this decision">view only</span>';

async function renderInbox(){
  const el=document.getElementById("inbox-body");if(!el)return;
  el.innerHTML='<div class="muted" style="padding:20px">Loading…</div>';
  const data=await loadInbox();
  const items=data.items||[];
  inboxBadge(items.length);
  if(!items.length){
    el.innerHTML='<div class="card" style="padding:28px;text-align:center" class="muted">🎉 Nothing waits on you.</div>';
    return;
  }
  items.sort((a,b)=>(b.escalated?1:0)-(a.escalated?1:0));
  let html="";
  for(const it of items){
    if(it.kind==="approve_ready"){
      html+=inboxCard("approve_ready",esc(it.ticket)+(it.priority?" · "+esc(it.priority):""),esc(it.title),
        ibtn("Review",`showTicket('${esc(it.ticket)}')`)+
        (canApproveReady()
          ?ibtn("Reject",`inboxAct('${esc(it.ticket)}','reject')`)+
           ibtn("Approve",`inboxAct('${esc(it.ticket)}','ready')`,1)
          :noRight()),it.ticket);
    }else if(it.kind==="verify"){
      html+=inboxCard("verify",esc(it.ticket),esc(it.title),
        ibtn("Evidence",`showTicket('${esc(it.ticket)}')`)+
        (canVerify()?ibtn("Verified",`inboxAct('${esc(it.ticket)}','verify')`,1):noRight()),it.ticket);
    }else if(it.kind==="assigned"){
      html+=inboxCard("assigned",esc(it.ticket)+" · "+esc(it.status||""),esc(it.title),
        ibtn("Return to agents",`inboxUnassign('${esc(it.ticket)}')`),it.ticket);
    }else if(it.kind==="question"){
      const ageMin=it.asked_at?Math.max(0,Math.round((Date.now()-new Date(it.asked_at))/60000)):null;
      const age=ageMin==null?"":(ageMin<60?` · waiting ${ageMin}m`:` · waiting ${Math.round(ageMin/60)}h`);
      const late=it.escalated?` <span style="color:var(--red);font-weight:700">past SLA</span>`:"";
      html+=inboxCard("question",esc(it.from)+(it.ticket?" · "+esc(it.ticket):"")+age+late,esc(it.body),
        ibtn("Answer in Scrum",`nav('discuss')`,1));
    }else if(it.kind==="auto_approved"){
      html+=inboxCard("auto_approved",esc(it.ticket)+" · undo for "+it.minutes_left+"m",esc(it.title),
        ibtn("Review",`showTicket('${esc(it.ticket)}')`)+
        (canApproveReady()?ibtn("Undo",`inboxUndo('${esc(it.ticket)}')`,1):noRight()),it.ticket);
    }else if(it.kind==="review_pr"){
      html+=inboxCard("review_pr","#"+it.number,esc(it.title),
        ibtn("Open review",`nav('review')`,1));
    }
  }
  el.innerHTML=html;
}

async function inboxAct(id,action){
  if(action==="reject"){
    const reason=await coxModal({title:"Reject "+id,message:"Lý do? (agents học từ đây — cùng lý do 2 lần là nó tự sửa trước khi hỏi lại)",input:{placeholder:"vd: thiếu acceptance criteria"},confirmText:"Reject"});
    if(reason===null||reason===undefined)return;
    try{await fetch(api("/ticket/"+encodeURIComponent(id)+"/reject"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({reason:String(reason||"")})});}catch(e){}
    renderInbox();return;
  }
  try{
    const r=await fetch(api("/ticket/"+encodeURIComponent(id)+"/"+action),{method:"POST",headers:{"Content-Type":"application/json"},body:"{}"});
    if(!r.ok){coxToast&&coxToast(await r.text());}
  }catch(e){}
  renderInbox();
}

async function inboxUndo(id){
  try{const r=await fetch(api("/ticket/"+encodeURIComponent(id)+"/undo-approval"),{method:"POST",headers:{"Content-Type":"application/json"},body:"{}"});
    if(!r.ok)toasty(await r.text(),"err");else toasty(id+" pulled back — that shape asks again","ok");
  }catch(e){}
  renderInbox();}

async function inboxUnassign(id){
  try{await fetch(api("/ticket/"+encodeURIComponent(id)+"/assign"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({username:""})});}catch(e){}
  renderInbox();
}

// Keep the badge honest even when the user lives in other tabs.
setInterval(async()=>{try{if(typeof PID!=="undefined"&&PID){const d=await loadInbox();inboxBadge((d.items||[]).length);}}catch(e){}},60000);
