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
  cost_approve:{label:"Approve the spend",ic:"ti-coin",col:"var(--amber)"},
  verify:{label:"Verify fix",ic:"ti-shield-check",col:"var(--green)"},
  assigned:{label:"Assigned to you",ic:"ti-user",col:"var(--purple)"},
  question:{label:"Question for you",ic:"ti-help-circle",col:"var(--amber)"},
  review_pr:{label:"PR held for human",ic:"ti-git-pull-request",col:"var(--teal)"},
  auto_approved:{label:"Auto-approved",ic:"ti-robot",col:"var(--dim)"},
  pr_stuck:{label:"PR stuck — needs you",ic:"ti-alert-triangle",col:"var(--red)"},
  human_eyes:{label:"Needs human eyes",ic:"ti-eye-exclamation",col:"var(--amber)"},
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

// The server decides who may ACT on each item (it carries `can_act` for the
// caller's role, and `role` = who this waits on). Everyone SEES every item;
// only the right role gets an enabled button. `can_act` is the authority — the
// endpoints 403 anyway — so there is no client-side role table to drift.
const noRight=(role)=>`<span class="ibx-noright" title="Only ${esc(role||'the right role')} may act on this">waiting on ${esc(role||'—')}</span>`;

// Inbox filter: "" = everything (default), "mine" = only what THIS user can
// act on right now. Persisted so a chosen view survives a refresh.
function inboxFilter(){return localStorage.getItem("coxinboxfilter")||"";}
function setInboxFilter(v){localStorage.setItem("coxinboxfilter",v||"");renderInbox();}

async function renderInbox(){
  const el=document.getElementById("inbox-body");if(!el)return;
  el.innerHTML='<div class="muted" style="padding:20px">Loading…</div>';
  const data=await loadInbox();
  const all=data.items||[];
  const mineN=all.filter(i=>i.can_act).length;
  inboxBadge(mineN);
  const flt=inboxFilter();
  const chip=(v,lbl,n)=>`<button class="ibx-chip${flt===v?' on':''}" onclick="setInboxFilter('${v}')">${lbl}${n!=null?` <span class="ibx-n">${n}</span>`:""}</button>`;
  const bar=`<div class="ibx-filters">${chip("","All",all.length)}${chip("mine","Assigned to me",mineN)}</div>`;
  const items=flt==="mine"?all.filter(i=>i.can_act):all;
  if(!all.length){
    el.innerHTML='<div class="empty" style="padding:48px 20px;text-align:center">🎉 Nothing waits on you — the team is fully unblocked.</div>';
    return;
  }
  items.sort((a,b)=>(b.escalated?1:0)-(a.escalated?1:0));
  let html=bar;
  if(!items.length){
    html+='<div class="empty" style="padding:24px;text-align:center">Nothing needs you right now — switch to <b>All</b> to see the team\'s queue.</div>';
    el.innerHTML=html;return;
  }
  for(const it of items){
    const act=!!it.can_act;
    if(it.kind==="approve_ready"){
      html+=inboxCard("approve_ready",esc(it.ticket)+(it.priority?" · "+esc(it.priority):""),esc(it.title),
        ibtn("Review",`showTicket('${esc(it.ticket)}')`)+
        (act
          ?ibtn("Reject",`inboxAct('${esc(it.ticket)}','reject')`)+
           ibtn("Approve",`inboxAct('${esc(it.ticket)}','ready')`,1)
          :noRight(it.role)),it.ticket);
    }else if(it.kind==="cost_approve"){
      const est=(typeof it.estimate_usd==="number")?" · ~$"+it.estimate_usd.toFixed(2):"";
      html+=inboxCard("cost_approve",esc(it.ticket)+(it.priority?" · "+esc(it.priority):"")+est,esc(it.title),
        ibtn("Review",`showTicket('${esc(it.ticket)}')`)+
        (act
          ?ibtn("Reject",`inboxAct('${esc(it.ticket)}','reject')`)+
           ibtn("Approve spend",`inboxAct('${esc(it.ticket)}','approve-cost')`,1)
          :noRight(it.role)),it.ticket);
    }else if(it.kind==="verify"){
      html+=inboxCard("verify",esc(it.ticket),esc(it.title),
        ibtn("Evidence",`showTicket('${esc(it.ticket)}')`)+
        (act
          ?ibtn("Send back",`inboxSendBack('${esc(it.ticket)}')`)+
           ibtn("Verified",`inboxAct('${esc(it.ticket)}','verify')`,1)
          :noRight(it.role)),it.ticket);
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
        (act?ibtn("Undo",`inboxUndo('${esc(it.ticket)}')`,1):noRight(it.role)),it.ticket);
    }else if(it.kind==="review_pr"){
      html+=inboxCard("review_pr","#"+it.number,esc(it.title),
        (act?ibtn("Open review",`nav('review')`,1):noRight(it.role)));
    }else if(it.kind==="human_eyes"){
      // The machine approved this PR but refuses to land it alone — the meta
      // line carries the gate's exact reason so the person decides informed.
      html+=inboxCard("human_eyes","#"+it.number+" · "+esc(it.reason||""),esc(it.title||("PR #"+it.number)),
        (it.url?ibtn("Open PR",`window.open('${esc(it.url)}','_blank')`):"")+
        (act
          ?ibtn("Dismiss",`inboxHumanPr(${it.number},'dismiss')`)+
           ibtn("Land it",`inboxHumanPr(${it.number},'approve')`,1)
          :noRight(it.role)));
    }else if(it.kind==="pr_stuck"){
      // The team tried, the SA rescued it, and it is still not moving. Say what
      // was tried and give the two moves a person actually has.
      const why=`#${it.number} · ${it.attempts} fix rounds · ${it.mergeable?"mergeable":"CONFLICTING"} · SA rescue failed`;
      html+=inboxCard("pr_stuck",why,esc(it.title),
        ibtn("Open on GitHub",`window.open('${esc(it.url)}','_blank')`)+
        (act?ibtn("Review queue",`nav('review')`,1):noRight(it.role)));
    }
  }
  el.innerHTML=html;
}

async function inboxAct(id,action){
  if(action==="reject"){
    const reason=await coxModal({title:"Reject "+id,message:"Why? (agents learn from this — the same reason twice and they fix it before asking again)",input:{placeholder:"e.g. missing acceptance criteria"},confirmText:"Reject"});
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

// The verify gate's other answer: the fix is not demonstrated. The reason
// goes on the ticket, which is what steers the next attempt.
async function inboxSendBack(id){
  const reason=await coxModal({title:"Send back "+id,
    message:"Why can't this be accepted yet? (the reason goes on the ticket — the agent reads it and redoes the work accordingly)",
    input:{placeholder:"e.g. no evidence for acceptance criteria #2"},confirmText:"Send back"});
  if(reason===null||reason===undefined)return;
  try{
    const r=await fetch(api("/ticket/"+encodeURIComponent(id)+"/send-back"),
      {method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({reason:String(reason||"")})});
    if(!r.ok){toasty(await r.text()||"Send back failed","err");return;}
    toasty(id+" sent back to the agents","ok");
  }catch(e){toasty("Network error","err");}
  renderInbox();
}

async function inboxUndo(id){
  try{const r=await fetch(api("/ticket/"+encodeURIComponent(id)+"/undo-approval"),{method:"POST",headers:{"Content-Type":"application/json"},body:"{}"});
    if(!r.ok)toasty(await r.text(),"err");else toasty(id+" pulled back — that shape asks again","ok");
  }catch(e){}
  renderInbox();}

// Decide a PR the machine held for human eyes: land it (this click IS the
// human the gate waited for) or dismiss the hold and handle it on the forge.
async function inboxHumanPr(number,action){
  try{
    const r=await fetch(api("/pr/"+number+"/human"),
      {method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({action})});
    if(!r.ok){toasty(await r.text()||"Failed","err");return;}
    toasty(action==="approve"?("PR #"+number+" landed"):("Hold on #"+number+" dismissed"),"ok");
  }catch(e){toasty("Network error","err");}
  renderInbox();
}

async function inboxUnassign(id){
  try{await fetch(api("/ticket/"+encodeURIComponent(id)+"/assign"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({username:""})});}catch(e){}
  renderInbox();
}

// Keep the badge honest even when the user lives in other tabs.
setInterval(async()=>{try{if(typeof PID!=="undefined"&&PID){const d=await loadInbox();inboxBadge((d.items||[]).filter(i=>i.can_act).length);}}catch(e){}},60000);
