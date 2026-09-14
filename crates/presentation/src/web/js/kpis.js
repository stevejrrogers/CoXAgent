// Overview KPI tiles with depth (CXA-F360): every tile carries a 14-day
// sparkline and a signed delta vs the prior 14 days, both computed client-side
// from state the 1 Hz snapshot already carries — history[] for ships/releases,
// tickets' created_at for filings, spend_history[] (+ the running day) for
// cost. No new endpoints, no chart library: the sparkline is pure inline SVG
// painted with theme tokens, so it remaps with the light/dark palette.
// Zero-state: a tile at 0 with no history shows a one-line hint of what fills
// it instead of a bare 0.
// `kpi`/KPI_IC moved here from core.js with the tile renderer that owns them;
// team/insights views keep calling the same globals (runtime refs — this file
// loads first, the functions it borrows load after, before first render).

const KPI_IC={Shipped:"ti-rocket","In flight":"ti-plane-tilt","Open bugs":"ti-bug",Documented:"ti-book",Releases:"ti-versions",Cost:"ti-coin","Total spend":"ti-coin",Tokens:"ti-cpu",Runs:"ti-repeat","Agent actions":"ti-bolt","Tickets shipped":"ti-rocket","Bugs open":"ti-bug","Team cost":"ti-coin"};
function kpi(k,v,sub){return `<div class="kpi"><div class="ic"><i class="ti ${KPI_IC[k]||'ti-point'}"></i></div><div class="v">${v}</div><div class="k">${k}${sub?` <span style="color:var(--green);font-weight:600">· ${sub}</span>`:""}</div></div>`;}

// --- pure series helpers ----------------------------------------------------

// UTC day key ("YYYY-MM-DD") of an RFC3339 timestamp; "" when unparsable.
function utcDay(ts){const s=String(ts||"");return /^\d{4}-\d{2}-\d{2}/.test(s)?s.slice(0,10):"";}

// The last `n` UTC day keys, oldest first — today is the newest bucket. Server
// timestamps and spend days are UTC, so the window is cut in UTC too.
function utcDayKeys(n){const out=[];for(let i=n-1;i>=0;i--)out.push(new Date(Date.now()-i*864e5).toISOString().slice(0,10));return out;}

// Per-day counts of `eventDays` over the `days` axis (a list of day keys);
// days with no events count 0.
function bucketDaily(eventDays,days){const m={};for(const d of eventDays){if(d)m[d]=(m[d]||0)+1;}return days.map(d=>m[d]||0);}

// Split a 28-day daily series into the current 14-day spark and its signed
// delta against the prior 14 days.
function window14(series28){const spark=series28.slice(14),prior=series28.slice(0,14);
  const sum=a=>a.reduce((x,y)=>x+y,0);return {spark,delta:sum(spark)-sum(prior)};}

function signedNum(n){return n>0?"+"+n:n<0?"-"+(-n):"±0";}
function signedMoney(n){return n>0?"+"+money(n):n<0?"-"+money(-n):"±0";}

// Inline-SVG sparkline over the 14 daily buckets — no library, theme tokens
// only (accent line + 13% tint area), so light and dark both stay legible.
function sparkSvg(vals){
  const w=132,h=30,pad=2,n=vals.length,max=Math.max(...vals,1);
  const pts=vals.map((v,i)=>[pad+i*(w-2*pad)/(n-1),h-pad-(v/max)*(h-2*pad)]);
  const line=pts.map(p=>p[0].toFixed(1)+","+p[1].toFixed(1)).join(" ");
  const last=pts[n-1];
  return `<svg class="spark" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" aria-hidden="true">`
    +`<polygon class="spark-a" points="${pad},${h-pad} ${line} ${w-pad},${h-pad}"/>`
    +`<polyline class="spark-l" points="${line}" vector-effect="non-scaling-stroke"/>`
    +`<circle class="spark-d" cx="${last[0].toFixed(1)}" cy="${last[1].toFixed(1)}" r="2.5"/></svg>`;
}

// One overview tile. Zero-state (value 0, no events in the whole 28-day
// lookback) swaps the big number for the one-line what-fills-this hint.
// Delta colour reinforces the sign glyph, never replaces it; Cost stays
// neutral on purpose — rising spend is a fact, not a win or a loss.
function overviewKpiTile(o){
  const dead=o.num===0&&o.series.every(v=>!v);
  const ic=`<div class="ic"><i class="ti ${KPI_IC[o.label]||'ti-point'}"></i></div>`;
  if(dead)return `<div class="kpi">${ic}<div class="vhint">${o.hint}</div><div class="k">${o.label}</div></div>`;
  const {spark,delta}=window14(o.series);
  // noSign: the delta is a companion COUNT (e.g. "132 filed"), not a change
  // of the headline number — a + glyph there implied "in flight grew by 132".
  const txt=o.money?signedMoney(delta):(o.noSign?String(Math.abs(delta)):signedNum(delta))+(o.deltaWord?" "+o.deltaWord:"");
  const cls=o.neutral?"kd":delta>0?"kd up":delta<0?"kd dn":"kd z";
  const go=o.go?` onclick="${o.go}" style="cursor:pointer" title="click to open"`:'';
  return `<div class="kpi"${go}>${ic}<div class="v">${o.text}</div><div class="k">${o.label} <span class="${cls}">${txt}</span> <span class="kwin">vs prior 14d</span></div><div title="${o.label} per day · last 14 days (UTC)">${sparkSvg(spark)}</div></div>`;
}

// The five overview tiles. Each series counts the per-day events that feed the
// tile's number; deltas compare the current 14-day window with the prior one.
//   Shipped   — history[] ship events of feature tickets (unknown type ⇒ feature,
//               same fallback metricsFrom uses)
//   In flight — tickets filed per day (created_at): the pipeline's inflow
//   Documented— history[] events of tickets that are documented NOW
//   Releases  — all history[] ship events
//   Cost      — closed spend days from spend_history plus the running day,
//               exactly the series the Cost view charts
function overviewKpis(s){
  const days=utcDayKeys(28);
  const byId={};(s.tickets||[]).forEach(t=>{byId[t.id]=t;});
  const isF=t=>!t||t.type!=="bug";
  const m=metricsFrom(s);
  const spend=s.spend||{};
  const shipDays=keep=>(s.history||[]).filter(keep).map(r=>utcDay(r.at));
  const usd={};(s.spend_history||[]).forEach(d=>{usd[d.day]=d.usd||0;});
  if(s.spend_day)usd[s.spend_day]=s.spend_today_usd||0;
  // Releases counts DISTINCT versions, not ship events: every shipped ticket
  // writes a history row carrying the version it landed in, so history.length
  // showed "202 releases" for ~30 actual tags (CXA-B171 — the number that
  // most read as fake). The series buckets each version's FIRST ship day.
  const verFirst={};
  (s.history||[]).forEach(r=>{const v=r.version;if(!v)return;const d=utcDay(r.at);
    if(!(v in verFirst)||d<verFirst[v])verFirst[v]=d;});
  const relCount=Object.keys(verFirst).length;
  return [
    overviewKpiTile({label:"Shipped",num:m.shipped,text:String(m.shipped),
      series:bucketDaily(shipDays(r=>isF(byId[r.ticket])),days),
      hint:"ships land here when a ticket reaches documented",go:"nav('board')"}),
    overviewKpiTile({label:"In flight",num:m.inflight,text:String(m.inflight),
      series:bucketDaily((s.tickets||[]).map(t=>utcDay(t.created_at)),days),
      hint:"work lands here when a ticket is readied for an agent",deltaWord:"filed",noSign:true,go:"nav('board')"}),
    overviewKpiTile({label:"Documented",num:m.docd,text:String(m.docd),
      series:bucketDaily(shipDays(r=>byId[r.ticket]&&byId[r.ticket].status==="documented"),days),
      hint:"docs land here when DOCS documents a shipped ticket",go:"nav('docs')"}),
    overviewKpiTile({label:"Releases",num:relCount,text:String(relCount),
      series:bucketDaily(Object.values(verFirst),days),
      hint:"releases land here when a version is tagged",
      go:"document.getElementById('ov-changelog').scrollIntoView({behavior:'smooth',block:'center'})"}),
    overviewKpiTile({label:"Cost",num:spend.total_cost_usd||0,text:money(spend.total_cost_usd),
      series:days.map(d=>usd[d]||0),
      hint:"cost accrues here as agent runs burn tokens",money:true,neutral:true,go:"nav('insights')"}),
  ].join("");
}
