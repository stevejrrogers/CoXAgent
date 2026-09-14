// CXA-F303 — approval-policy transparency: what the adaptive gate learned,
// what it is deciding alone today, and the per-shape always-ask switch.
// Extends Settings → Workflow → Approval (the human-gate section). Classic
// script, one shared scope (see core.js); the data comes from
// GET /api/projects/:pid/approval-policy and the flips POST to
// approval-policy/ask-again and approval-policy/release. Undo reuses the
// inbox's per-ticket undo-approval endpoint — one undo action, one home.

// Rule chip: how the gate treats a shape today. effective decides the colour:
// "auto" means the loop promotes without a person, "ask" means it does not.
function policyRuleChip(row){
  const L=row.learned||"keep_asking";
  let label,ic,col;
  if(row.overridden){label="ALWAYS ASK (you overrode)";ic="ti-ban";col="var(--purple)";}
  else if(L.auto_approve){label="AUTO-APPROVE";ic="ti-robot";col="var(--green)";}
  else if(L.preflight_fix){label="PREFLIGHT FIX";ic="ti-adjustments-horizontal";col="var(--amber)";}
  else{label="KEEP ASKING";ic="ti-shield-check";col="var(--muted)";}
  return `<span style="display:inline-flex;align-items:center;gap:5px;font-size:10px;font-weight:700;letter-spacing:.4px;text-transform:uppercase;color:${col};background:color-mix(in srgb,${col} 13%,transparent);border-radius:20px;padding:2px 8px"><i class="ti ${ic}" style="font-size:12px"></i>${label}</span>`;
}

// Driver chip for a ticket the loop is deciding alone (learned rule vs risk
// heuristic — the distinction the announcement only implies).
function policyDriverChip(d){
  const M={learned:["LEARNED RULE","ti-robot","var(--green)"],preflight:["PREFLIGHT MET","ti-adjustments-horizontal","var(--amber)"],risk:["RISK HEURISTIC","ti-gauge","var(--accent2)"],override:["YOUR OVERRIDE","ti-ban","var(--purple)"]};
  const [label,ic,col]=M[d.driver]||["RISK HEURISTIC","ti-gauge","var(--accent2)"];
  return `<span style="display:inline-flex;align-items:center;gap:5px;font-size:10px;font-weight:700;letter-spacing:.4px;text-transform:uppercase;color:${col};background:color-mix(in srgb,${col} 13%,transparent);border-radius:20px;padding:2px 8px"><i class="ti ${ic}" style="font-size:12px"></i>${label}</span>`;
}

function policyRow(icon,col,left,right){
  return `<div style="display:flex;gap:12px;align-items:center;padding:10px 14px;border-bottom:1px solid var(--border)">
    <div style="width:30px;height:30px;border-radius:8px;background:color-mix(in srgb,${col} 13%,transparent);display:flex;align-items:center;justify-content:center;flex-shrink:0"><i class="ti ${icon}" style="font-size:15px;color:${col}"></i></div>
    <div style="min-width:0;flex:1">${left}</div>
    <div style="display:flex;gap:8px;align-items:center;flex-shrink:0">${right}</div></div>`;
}

function policyShapeLabel(shape){
  return `<span style="font-family:ui-monospace,Menlo,monospace;font-size:11.5px;font-weight:600;background:var(--card);border:1px solid var(--border);border-radius:20px;padding:2px 8px">${esc(shape)}</span>`;
}

function policySection(icon,title,rows,empty){
  if(!rows)return "";
  return `<div style="font-size:12px;font-weight:600;color:var(--muted);padding:10px 14px 2px;display:flex;align-items:center;gap:6px"><i class="ti ${icon}" style="font-size:14px;color:var(--accent2)"></i>${title}</div>`
    +(rows.length?rows.join(""):`<div style="padding:6px 14px 12px;font-size:12.5px;color:var(--dim)">${empty}</div>`);
}

async function renderApprovalPolicy(){
  const el=document.getElementById("approval-policy-panel");if(!el)return;
  el.innerHTML='<div style="padding:14px;color:var(--dim);font-size:12.5px">Loading what the gate learned…</div>';
  let d=null;
  try{const r=await fetch(api("/approval-policy"));if(r.ok)d=await r.json();}catch(e){}
  if(!d){el.innerHTML='<div style="padding:14px;color:var(--dim);font-size:12.5px">The policy surface could not be reached.</div>';return;}
  const when=s=>s?new Date(s).toLocaleString():"—";
  const flip=(shape,release)=>`<button class="tk-btn${release?'':' go'}" onclick="flipApprovalPolicy('${esc(shape)}',${!!release})">${release?"Release — re-learn":"Always ask"}</button>`;

  // AC5: the gate off is an explicit state, not an empty panel.
  if(d.gate_off){
    el.innerHTML=`<div style="display:flex;gap:12px;align-items:center;padding:16px 14px">
      <div style="width:34px;height:34px;border-radius:10px;background:color-mix(in srgb,var(--muted) 14%,transparent);display:flex;align-items:center;justify-content:center;flex-shrink:0"><i class="ti ti-robot-off" style="font-size:18px;color:var(--muted)"></i></div>
      <div><div style="font-size:13.5px;font-weight:600">Adaptive gate is off</div>
      <div style="font-size:12.5px;color:var(--muted)">No shape is auto-approved and no learned rule applies. ${d.enabled===false?"Turn Auto-approve on above":"The Ready gate is off — designed tickets skip approval entirely"}.</div></div></div>`;
    return;
  }

  // Rule rows: overridden shapes first (the read model sorts them so).
  const shapeRows=(d.shapes||[]).map(r=>policyRow(
    r.overridden?"ti-ban":"ti-robot",
    r.overridden?"var(--purple)":(r.effective==="auto"?"var(--green)":"var(--muted)"),
    `<div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap">${policyShapeLabel(r.shape)}${policyRuleChip(r)}</div>
     <div style="font-size:11.5px;color:var(--dim);margin-top:3px">
       ${r.samples.approve+r.samples.reject+r.samples.undo} sample${(r.samples.approve+r.samples.reject+r.samples.undo)===1?"":"s"} (${r.samples.approve} approve · ${r.samples.reject} reject · ${r.samples.undo} undo)
       ${r.deciders.length?` · decided by ${r.deciders.map(esc).join(", ")}`:""}
       ${r.last_decision_at?` · last ${when(r.last_decision_at)}`:""}
       ${r.last_reason?` — “${esc(r.last_reason)}”`:""}</div>`,
    flip(r.shape,r.overridden)
  )).join("");

  // What the loop is deciding alone today: driver (learned vs risk), the
  // announcement's why + risk score.
  const deciderows=(d.deciding||[]).map(x=>policyRow(
    x.will_auto?"ti-plane-tilt":"ti-shield-check",
    x.will_auto?"var(--accent2)":"var(--muted)",
    `<div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap">
       <span style="font-family:ui-monospace,Menlo,monospace;font-size:11.5px;color:var(--muted)">${esc(x.ticket)}</span>
       ${policyDriverChip(x)}${policyShapeLabel(x.shape)}</div>
     <div style="font-size:12.5px;font-weight:600;margin-top:3px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(x.title)}</div>
     <div style="font-size:11.5px;color:var(--dim);margin-top:2px">risk ${x.risk_score} · ${esc(x.risk_why)}${x.will_auto?" — auto-approves on the next pass":""}</div>`,
    ""
  )).join("");

  // Live undo window (entries surfaced from the inbox's auto-approved items)
  // and the expired history, attributed per shape.
  const undoRows=(d.undoable||[]).map(u=>policyRow(
    "ti-clock","var(--amber)",
    `<div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap">
       <span style="font-family:ui-monospace,Menlo,monospace;font-size:11.5px;color:var(--muted)">${esc(u.ticket)}</span>${policyShapeLabel(u.shape)}</div>
     <div style="font-size:12.5px;font-weight:600;margin-top:3px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(u.title)}</div>`,
    `<span style="font-size:11.5px;font-weight:700;color:var(--amber)">${u.minutes_left}m left</span><button class="tk-btn" onclick="undoApprovalPolicy('${esc(u.ticket)}')">Undo</button>`
  )).join("");
  const historyRows=(d.expired||[]).map(x=>policyRow(
    "ti-history","var(--dim)",
    `<div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap">
       <span style="font-family:ui-monospace,Menlo,monospace;font-size:11.5px;color:var(--muted)">${esc(x.ticket)}</span>${policyShapeLabel(x.shape)}</div>
     <div style="font-size:12.5px;font-weight:600;margin-top:3px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(x.title)}</div>
     <div style="font-size:11.5px;color:var(--dim)">auto-approved ${when(x.approved_at)} · window closed</div>`,
    ""
  )).join("");

  el.innerHTML=
    policySection("ti-robot","Rules the gate learned per ticket shape",shapeRows,"No shapes yet — approve or undo a few designed tickets and what the team decides shows up here.")
   +policySection("ti-gauge","Deciding alone this cycle",deciderows,"Nothing is waiting behind the approval gate right now.")
   +policySection("ti-clock","Inside the undo window",undoRows,"No auto-approval is undoable right now.")
   +(historyRows.length?policySection("ti-history","Past the undo window",historyRows,""):"");
}

// AC2: force a shape back to always-ask — the next cycle pass honours it from
// freshly loaded state, no restart — or release the override so the gate
// re-learns from the recorded decisions (history is never zeroed).
async function flipApprovalPolicy(shape,release){
  try{
    const r=await fetch(api("/approval-policy/"+(release?"release":"ask-again")),
      {method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({shape})});
    if(!r.ok){
      // The 400 carries the shape vocabulary — the one message that tells the
      // operator what to type instead.
      const b=await r.json().catch(()=>null);
      toasty((b&&b.error)||"Flip failed","err");return;
    }
    toasty(release?("`"+shape+"` released — the gate re-learns"):("`"+shape+"` goes back to always-ask"),"ok");
  }catch(e){toasty("Network error","err");}
  renderApprovalPolicy();
}

// AC4: same undo action the inbox offers — one endpoint, one meaning.
async function undoApprovalPolicy(id){
  try{
    const r=await fetch(api("/ticket/"+encodeURIComponent(id)+"/undo-approval"),
      {method:"POST",headers:{"Content-Type":"application/json"},body:"{}"});
    if(!r.ok){toasty(await r.text()||"Undo failed","err");return;}
    toasty(id+" pulled back — that shape asks again","ok");
  }catch(e){toasty("Network error","err");}
  renderApprovalPolicy();
  if(typeof renderInbox==="function")renderInbox();
}
