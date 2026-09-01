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
  reverted_work:{label:"Reverted work — confirm or dismiss",ic:"ti-arrow-back-up",col:"var(--red)"},
  on_hold:{label:"On hold — resume when unblocked",ic:"ti-player-pause",col:"var(--amber)"},
};

function inboxCard(kind,meta,title,actions,ticket){
  const k=INBOX_KIND[kind]||{label:kind,ic:"ti-inbox",col:"var(--muted)"};
  const open=ticket?`onclick="showTicket('${esc(ticket)}')"`:"";
  return `<div class="panel ibx-card" ${open} style="margin-bottom:12px;display:flex;gap:14px;align-items:center;${ticket?'cursor:pointer;':''}transition:border-color .15s" onmouseover="this.style.borderColor='${k.col}'" onmouseout="this.style.borderColor='var(--border)'">
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
  // Held-for-digest questions (CXA-F176) wait on me, but deliberately do not
  // count as fresh interrupts — they surface in one batch at the window end.
  // On-hold tickets render as ONE collapsed card, so they must COUNT as one:
  // a badge saying 191 over an inbox showing 6 cards reads as a bug (and was
  // reported as one). Parked work is a single standing decision, not N.
  const held=all.filter(i=>i.kind==="on_hold");
  const rest=all.filter(i=>i.kind!=="on_hold");
  const mineN=rest.filter(i=>i.can_act&&!i.deferred).length+(held.some(i=>i.can_act)?1:0);
  inboxBadge(mineN);
  const flt=inboxFilter();
  const chip=(v,lbl,n)=>`<button class="ibx-chip${flt===v?' on':''}" onclick="setInboxFilter('${v}')">${lbl}${n!=null?` <span class="ibx-n">${n}</span>`:""}</button>`;
  const bar=`<div class="ibx-filters">${chip("","All",rest.length+(held.length?1:0))}${chip("mine","Assigned to me",mineN)}</div>`;
  const items=flt==="mine"?all.filter(i=>i.can_act):all;
  if(!all.length){
    el.innerHTML='<div class="empty" style="padding:48px 20px;text-align:center">🎉 Nothing waits on you — the team is fully unblocked.</div>';
    return;
  }
  // Escalated first, then live items, held-for-digest ones last: the queue
  // reads in interruption order, queued-for-digest at the bottom.
  items.sort((a,b)=>(b.escalated?1:0)-(a.escalated?1:0)||(a.deferred?1:0)-(b.deferred?1:0));
  let html=bar;
  // On-hold tickets collapse into ONE card: the auto-hold sweep can park a
  // hundred exhausted tickets at once, and a card per ticket buries the items
  // that actually need a decision today. The board's status filter is the
  // right place to browse them.
  const heldCards=items.filter(i=>i.kind==="on_hold");
  if(heldCards.length){
    const sample=heldCards.slice(0,3).map(h=>esc(h.ticket)).join(", ");
    html+=inboxCard("on_hold",`${heldCards.length} ticket${heldCards.length===1?"":"s"} parked · e.g. ${sample}`,
      "Blocked on the outside world — resume each from its ticket when unblocked",
      ibtn("View on board",`SF='on_hold';nav('board');setWorkTab('board')`,1));
  }
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
      // Static evidence says the fix worked; the live instance (CXA-F242-C)
      // lets the reviewer actually SEE it run. The card carries the URL only
      // when the project's deploy port resolves — otherwise no control at all,
      // never a dead button. The control is a real anchor (CXA-F247): the
      // open-in-new-tab contract lives on the element (target/rel), and the
      // href only ever receives an https?:// URL — the whitelist runs BEFORE
      // any markup, so a javascript:/data: scheme or a protocol-relative
      // //host can never ride the click. Send back cites the same URL so the
      // refusal reason can reference what was actually seen.
      const liveUrl=(it.reproduce_url&&/^https?:\/\//i.test(it.reproduce_url))?it.reproduce_url:"";
      // Engine & model provenance (CXA-F257): what actually produced the work
      // this card asks the reviewer to approve — the most recent step's
      // attempts, "model unknown" marked explicitly, never a blank field.
      const prov=(it.provenance||[]).map(provChip).join(" ");
      html+=inboxCard("verify",esc(it.ticket)+(prov?" "+prov:""),esc(it.title),
        (liveUrl?`<a class="tk-btn ibx-btn" href="${escAttr(liveUrl)}" target="_blank" rel="noopener noreferrer">Open live preview</a>`:"")+
        ibtn("Evidence",`showTicket('${esc(it.ticket)}')`)+
        (act
          ?ibtn("Send back",`inboxSendBack('${esc(it.ticket)}','${escAttr(liveUrl)}')`)+
           ibtn("Verified",`inboxAct('${esc(it.ticket)}','verify')`,1)
          :noRight(it.role)),it.ticket);
    }else if(it.kind==="on_hold"){
      continue; // collapsed into the single summary card above
    }else if(it.kind==="assigned"){
      html+=inboxCard("assigned",esc(it.ticket)+" · "+esc(it.status||""),esc(it.title),
        ibtn("Return to agents",`inboxUnassign('${esc(it.ticket)}')`),it.ticket);
    }else if(it.kind==="question"){
      const ageMin=it.asked_at?Math.max(0,Math.round((Date.now()-new Date(it.asked_at))/60000)):null;
      const age=ageMin==null?"":(ageMin<60?` · waiting ${ageMin}m`:` · waiting ${Math.round(ageMin/60)}h`);
      const late=it.escalated?` <span style="color:var(--red);font-weight:700">past SLA</span>`:"";
      // Held for the owner's focus-window digest (CXA-F176): visibly queued,
      // not a fresh interrupt — answering early is still allowed.
      const held=it.deferred?` <span style="font-size:10px;font-weight:700;letter-spacing:.4px;text-transform:uppercase;color:var(--dim);background:color-mix(in srgb,var(--dim) 12%,transparent);border:1px solid var(--border);border-radius:20px;padding:2px 8px">held for digest</span>`:"";
      html+=inboxCard("question",esc(it.from)+(it.ticket?" · "+esc(it.ticket):"")+age+late+held,esc(it.body),
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
    }else if(it.kind==="reverted_work"){
      // A scan suspected shipped work was undone (CXA-F047). The meta line
      // carries the shipping ticket and role; confirming it is what allows
      // planning to learn — dismissing it marks the suspicion a false one.
      html+=inboxCard("reverted_work",esc(it.ticket)+" · "+esc(it.role||"")+" · "+esc((it.at||"").slice(0,10)),esc(it.subject),
        ibtn("Open ticket",`showTicket('${esc(it.ticket)}')`)+
        (act
          ?ibtn("Dismiss",`inboxRevert('${esc(it.sha)}','dismiss')`)+
           ibtn("Confirm revert",`inboxRevert('${esc(it.sha)}','approve')`,1)
          :noRight(it.role)),it.ticket);
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
// goes on the ticket, which is what steers the next attempt. `url` is the
// live instance the reviewer was shown (CXA-F247) — cited in the dialog so
// the refusal reason can reference what was actually seen.
async function inboxSendBack(id,url){
  const reason=await coxModal({title:"Send back "+id,
    message:"Why can't this be accepted yet? (the reason goes on the ticket — the agent reads it and redoes the work accordingly)"+
      (url?" Live instance reviewed: "+url:""),
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

// Decide a detected revert (CXA-F047): confirm the shipped work really was
// undone — the only verdict planning is allowed to learn from — or dismiss
// the suspicion (a non-code revert, e.g. a docs or CI bump).
async function inboxRevert(sha,action){
  try{
    const r=await fetch(api("/reverts/"+encodeURIComponent(sha)),
      {method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({action})});
    if(!r.ok){toasty(await r.text()||"Failed","err");return;}
    toasty(action==="approve"?"Revert confirmed — planning will weigh it":"Revert dismissed — not counted","ok");
  }catch(e){toasty("Network error","err");}
  renderInbox();
  if(typeof CUR!=="undefined"&&(CUR==="overview"||CUR==="board"))renderActive();
}

async function inboxUnassign(id){
  try{await fetch(api("/ticket/"+encodeURIComponent(id)+"/assign"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({username:""})});}catch(e){}
  renderInbox();
}

// Keep the badge honest even when the user lives in other tabs. Held-for-
// digest questions (CXA-F176) do not count — they batch into one flush.
setInterval(async()=>{try{if(typeof PID!=="undefined"&&PID){const d=await loadInbox();inboxBadge((d.items||[]).filter(i=>i.can_act&&!i.deferred).length);}}catch(e){}},60000);


// ---- Inbox keyboard shortcuts: j/k move the selection, Enter opens the
// ticket, a fires the card's PRIMARY action, d its dismiss/reject-style
// secondary. Active only while the Inbox view is on screen and no input has
// focus, so typing elsewhere never triggers approvals.
let IBX_SEL=-1;
function ibxCards(){return Array.from(document.querySelectorAll('#view-inbox .ibx-card'));}
function ibxPaint(){
  ibxCards().forEach((c,i)=>{c.style.outline=i===IBX_SEL?'2px solid var(--accent2)':'none';
    if(i===IBX_SEL)c.scrollIntoView({block:'nearest'});});
}
document.addEventListener('keydown',e=>{
  if(typeof CUR==='undefined'||CUR!=='inbox')return;
  const t=e.target;
  if(t&&(t.tagName==='INPUT'||t.tagName==='TEXTAREA'||t.isContentEditable))return;
  if(e.metaKey||e.ctrlKey||e.altKey)return;
  const cards=ibxCards();if(!cards.length)return;
  if(e.key==='j'){IBX_SEL=Math.min(cards.length-1,IBX_SEL+1);ibxPaint();e.preventDefault();}
  else if(e.key==='k'){IBX_SEL=Math.max(0,IBX_SEL-1);ibxPaint();e.preventDefault();}
  else if(IBX_SEL>=0&&IBX_SEL<cards.length){
    const card=cards[IBX_SEL];
    if(e.key==='Enter'){card.click();e.preventDefault();}
    else if(e.key==='a'){const b=card.querySelector('.ibx-pri');if(b){b.click();e.preventDefault();}}
    else if(e.key==='d'){const bs=Array.from(card.querySelectorAll('.ibx-btn:not(.ibx-pri)'));
      const d=bs.find(x=>/dismiss|reject|hold|defer/i.test(x.textContent))||bs[0];
      if(d){d.click();e.preventDefault();}}
  }
});
