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

function inboxCard(inner,actions,ticket){
  // The whole card opens the full ticket dialog (description, AC, specs,
  // comments) — nobody should approve from a title alone.
  const open=ticket?`onclick="showTicket('${esc(ticket)}')" style="cursor:pointer"`:"";
  return `<div class="card" ${open} style="margin-bottom:10px;padding:14px;display:flex;justify-content:space-between;gap:12px;align-items:center;cursor:${ticket?'pointer':'default'}">
    <div style="min-width:0">${inner}</div>
    <div style="display:flex;gap:8px;flex-shrink:0" onclick="event.stopPropagation()">${actions}</div></div>`;
}

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
  const label={approve_ready:"⏳ Approve to Ready",verify:"🧪 Verify fix",assigned:"🧑‍💻 Assigned to you",question:"❓ Question for you",review_pr:"👀 PR held for human"};
  let html="";
  for(const it of items){
    if(it.kind==="approve_ready"){
      html+=inboxCard(
        `<b>${label[it.kind]}</b> · <span class="muted">${esc(it.ticket)}</span><br>${esc(it.title)}`,
        `<button onclick="showTicket('${esc(it.ticket)}')">Review</button>
         <button class="pri" onclick="inboxAct('${esc(it.ticket)}','ready')">Approve</button>
         <button onclick="inboxAct('${esc(it.ticket)}','reject')">Reject</button>`,it.ticket);
    }else if(it.kind==="verify"){
      html+=inboxCard(
        `<b>${label[it.kind]}</b> · <span class="muted">${esc(it.ticket)}</span><br>${esc(it.title)}`,
        `<button onclick="showTicket('${esc(it.ticket)}')">Review evidence</button>
         <button class="pri" onclick="inboxAct('${esc(it.ticket)}','verify')">Verified</button>`,it.ticket);
    }else if(it.kind==="assigned"){
      html+=inboxCard(
        `<b>${label[it.kind]}</b> · <span class="muted">${esc(it.ticket)} · ${esc(it.status||"")}</span><br>${esc(it.title)}`,
        `<button onclick="inboxUnassign('${esc(it.ticket)}')">Return to agents</button>`,it.ticket);
    }else if(it.kind==="question"){
      const ageMin=it.asked_at?Math.max(0,Math.round((Date.now()-new Date(it.asked_at))/60000)):null;
      const age=ageMin==null?"":(ageMin<60?` · waiting ${ageMin}m`:` · waiting ${Math.round(ageMin/60)}h`);
      const late=it.escalated?` <span style="color:var(--red);font-weight:700">· past SLA</span>`:"";
      html+=inboxCard(
        `<b>${label[it.kind]}</b> · <span class="muted">${esc(it.from)}${it.ticket?" · "+esc(it.ticket):""}${age}</span>${late}<br>${esc(it.body)}`,
        `<button class="pri" onclick="nav('discuss')">Answer in Scrum</button>`);
    }else if(it.kind==="review_pr"){
      html+=inboxCard(
        `<b>${label[it.kind]}</b> · <span class="muted">#${it.number}</span><br>${esc(it.title)}`,
        `<button class="pri" onclick="nav('review')">Open review</button>`);
    }
  }
  el.innerHTML=html;
}

async function inboxAct(id,action){
  try{
    const r=await fetch(api("/ticket/"+encodeURIComponent(id)+"/"+action),{method:"POST",headers:{"Content-Type":"application/json"},body:"{}"});
    if(!r.ok){coxToast&&coxToast(await r.text());}
  }catch(e){}
  renderInbox();
}

async function inboxUnassign(id){
  try{await fetch(api("/ticket/"+encodeURIComponent(id)+"/assign"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({username:""})});}catch(e){}
  renderInbox();
}

// Keep the badge honest even when the user lives in other tabs.
setInterval(async()=>{try{if(typeof PID!=="undefined"&&PID){const d=await loadInbox();inboxBadge((d.items||[]).length);}}catch(e){}},60000);
