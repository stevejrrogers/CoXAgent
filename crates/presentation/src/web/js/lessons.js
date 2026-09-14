// CXA-F306 — Lesson efficacy loop: the Hub lessons recurrence view.
//
// Lessons were write-only: recorded, ranked into briefs, never measured. This
// panel reads the additive `lesson_efficacy` key on /metrics/summary (the
// CXA-F230 attention pattern: 60s project-keyed cache, zero gates render
// NOTHING so the overview is untouched until a first lesson lands) and turns
// the ledger into the reviewer's surface:
//   * every lesson: when it was recorded, its recurrence count, its most
//     recent recurrence, and the linked incidents;
//   * lessons with 2+ recurrences in a "Repeating" section with the one-click
//     escalation paths that already exist — file a prevention ticket, or
//     dismiss a suggested incident-to-lesson match (a dismissed match never
//     increments again);
//   * shipped lessons (CXA-F371, source "shipped" — seeded with the binary on
//     first boot) are badged apart from locally learned ones.

function loadLessonEfficacy(){
  // Cache keyed by project: switching projects must never show the previous
  // project's efficacy data for the rest of the cache window.
  if(window._effPid!==PID){window._eff=null;window._effAt=0;}
  if(window._effAt&&Date.now()-window._effAt<60000){renderLessonEfficacy();return;}
  window._effPid=PID;window._effAt=Date.now();
  fetch(api("/metrics/summary")).then(r=>r.json()).then(d=>{
    window._eff=(d&&d.lesson_efficacy)?d.lesson_efficacy:null;
    if(CUR==="overview")renderLessonEfficacy();
  }).catch(()=>{window._eff=null;});
}
function renderLessonEfficacy(){
  const el=document.getElementById("ov-lessons");if(!el)return;
  const eff=window._eff;
  const rows=(eff&&eff.lessons)||[];
  // Zero gates render nothing: a project with NO recorded lessons reads
  // exactly as before this panel existed. Every TRACKED lesson renders (AC2:
  // "for every lesson"), repeating ones first.
  if(!rows.length){setHTML(el,"");return;}
  const day=iso=>iso?String(iso).slice(0,10):"—";
  const repeating=rows.filter(l=>l.repeating), watched=rows.filter(l=>!l.repeating);
  const incidentsHtml=l=>{
    const recent=(l.incidents||[]).slice(-3).reverse();
    if(!recent.length)return "";
    return recent.map(inc=>`<div style="display:flex;align-items:center;gap:6px;font-size:11px;color:var(--muted);margin-top:2px">
      <i class="ti ti-bolt" style="font-size:12px;color:var(--amber)"></i>
      <span style="min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(inc.incident_reason||"incident")} · ${day(inc.incident_at)}</span>
      <a style="color:var(--dim);cursor:pointer;text-decoration:underline" onclick="dismissLessonMatch(this)" data-lesson="${escAttr(l.text)}" data-incident-at="${escAttr(inc.incident_at)}" data-incident-reason="${escAttr(inc.incident_reason||"")}">dismiss</a>
    </div>`).join("");
  };
  const actionHtml=l=>l.escalated
    ?`<span style="flex:none;display:inline-flex;align-items:center;gap:5px;font-size:10px;font-weight:700;text-transform:uppercase;letter-spacing:.4px;padding:2px 8px;border-radius:10px;background:var(--card2);color:var(--muted)" title="structural work escalated">
       <i class="ti ti-${l.stage==="bug"?"bug":"tool"}" style="color:${l.stage==="bug"?"var(--red)":"var(--accent2)"}"></i>${esc(l.stage||"chore")} ${esc(l.structural_ticket||"")}</span>`
    :`<button class="gc-btn pri" style="flex:none;padding:6px 12px;font-size:12px" onclick="escalateLesson(this)" data-lesson="${escAttr(l.text)}">File prevention ticket</button>`;
  // Shipped bootstrap lessons (CXA-F371) carry source:"shipped" on the row;
  // locally learned lessons have no source and render without the badge.
  const shippedBadge=l=>l.source==="shipped"
    ?`<span style="display:inline-flex;align-items:center;gap:4px;font-size:10px;font-weight:700;text-transform:uppercase;letter-spacing:.4px;padding:1px 8px;border-radius:10px;background:var(--card2);color:var(--accent2);margin-right:7px;vertical-align:1px" title="Shipped with the binary — the accumulated lesson base every fresh install starts with (CXA-F371)"><i class="ti ti-package" style="font-size:11px"></i>shipped</span>`
    :"";
  const row=l=>`<div style="display:flex;align-items:flex-start;gap:12px;padding:10px 2px;border-bottom:1px solid var(--border)">
    <div style="flex:1;min-width:0">
      <div style="font-size:13px;line-height:1.55;color:var(--text)">${shippedBadge(l)}${esc(l.text)}</div>
      <div style="font-size:11px;color:var(--dim);margin-top:4px;font-family:ui-monospace,Menlo,monospace">
        recorded ${day(l.recorded_at)} · ${l.recurrence_count} recurrence${l.recurrence_count===1?"":"s"} · last ${day(l.last_recurrence_at)}${l.re_recordings?` · re-learned ${l.re_recordings}×`:""}
      </div>
      ${incidentsHtml(l)}
    </div>
    ${actionHtml(l)}
  </div>`;
  const repSec=repeating.length
    ?`<div style="font-size:12px;font-weight:600;color:var(--red);text-transform:uppercase;letter-spacing:.6px;margin-bottom:4px">Repeating — file structural fixes</div>${repeating.map(row).join("")}`:"";
  const watchedRows=watched;
  const watchedSec=watchedRows.length
    ?`<div style="font-size:11px;color:var(--dim);margin:14px 0 4px">Watched — recorded, no repeat yet</div>${watchedRows.map(row).join("")}`:"";
  setHTML(el,`<div class="sec" style="margin-top:22px">Lesson efficacy <span style="font-size:11px;color:var(--dim);font-weight:400">· whether each lesson actually stopped its failure class — ${eff.repeaters} repeating</span></div>
    <div class="panel">${repSec}${watchedSec}</div>`);
}
function dismissLessonMatch(a){
  const body={lesson:a.dataset.lesson,incident_at:a.dataset.incidentAt,incident_reason:a.dataset.incidentReason};
  fetch(api("/lessons/dismiss"),{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify(body)})
    .then(()=>{window._effAt=0;loadLessonEfficacy();}).catch(()=>{});
}
function escalateLesson(b){
  b.disabled=true;
  fetch(api("/lessons/escalate"),{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({lesson:b.dataset.lesson})})
    .then(()=>{window._effAt=0;loadLessonEfficacy();}).catch(()=>{b.disabled=false;});
}
