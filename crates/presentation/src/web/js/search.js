// Global search palette (CXA-F275): one box across tickets, wiki pages and
// chat threads. Cmd/Ctrl+K from every view (the rail's search button too).
// The three big corpora are SERVER-backed via /api/search so hits cover every
// channel, thread and the whole backlog — not just what this tab already
// loaded. Channels/people/projects stay client-side: that data is already in
// memory and the server has nothing better. Extracted from chat.js so the box
// used everywhere has one home instead of a corner of a 2,600-line module.
let GS_TIMER=null,GS_ABORT=null,GS_HITS=[],GS_Q="",GS_PENDING=false,
    GS_SEL=-1,GS_KIND="all";
// Groups the "Show more" chip has expanded — keyed per section, resets on a
// new query. The server caps each kind at 8, so expansion is bounded too.
const GS_EXPAND=new Set();
const GS_PAGE=4; // rows per group before "Show more"
const GS_ICON={ticket:"ticket",page:"file-text",comment:"message-circle-2",message:"message-2",channel:"hash",person:"user",project:"folder"};
// esc() covers element text; data attributes are double-quoted, so the quote
// itself needs escaping too — ids can be user-influenced (doc ids via PUT).
const gsAttr=s=>esc(s).replace(/"/g,"&quot;");

function openGlobalSearch(){
  const ov=document.getElementById("ov-chatsearch");
  document.getElementById("chatsearch-input").value="";
  GS_HITS=[];GS_Q="";GS_PENDING=false;GS_SEL=-1;GS_KIND="all";GS_EXPAND.clear();
  if(GS_ABORT)GS_ABORT.abort();
  gsChips();
  document.getElementById("chatsearch-list").innerHTML='<div class="empty">Type to search tickets, wiki and chat…</div>';
  ov.classList.add("open");
  setTimeout(()=>document.getElementById("chatsearch-input").focus(),40);
}
function closeChatSearch(){
  if(GS_ABORT)GS_ABORT.abort();
  document.getElementById("ov-chatsearch").classList.remove("open");
}

// Scope chips (All / Tickets / Wiki / Chat) — a client-side filter over the
// grouped results, not a second query.
function gsChips(){
  const box=document.getElementById("cs-chips");if(!box)return;
  const chip=(k,label)=>`<button type="button" class="cs-chip${GS_KIND===k?" on":""}" onclick="gsScope('${k}')">${label}</button>`;
  box.innerHTML=chip("all","All")+chip("ticket","Tickets")+chip("page","Wiki")+chip("chat","Chat");
}
function gsScope(k){GS_KIND=k;GS_SEL=-1;chatSearchRun();}

// oninput handler (index.html): client legs render instantly, the server pass
// refines debounced. Short queries never fetch (AC4) — the explicit hint shows.
function chatSearchRun(){
  const q=document.getElementById("chatsearch-input").value.trim();
  GS_Q=q;
  clearTimeout(GS_TIMER);
  if(GS_ABORT)GS_ABORT.abort();
  if(q.length<2){GS_PENDING=false;GS_EXPAND.clear();gsRender();return;}
  GS_PENDING=true;
  gsRender();
  GS_ABORT=new AbortController();
  const signal=GS_ABORT.signal;
  GS_TIMER=setTimeout(()=>{
    const pid=(typeof PID!=="undefined"&&PID)?`&pid=${encodeURIComponent(PID)}`:"";
    fetch(`/api/search?q=${encodeURIComponent(q)}${pid}`,{signal})
      .then(r=>{if(!r.ok)throw new Error(r.status);return r.json();})
      .then(hits=>{if(GS_Q!==q)return;GS_HITS=Array.isArray(hits)?hits:[];GS_PENDING=false;GS_SEL=-1;gsRender();})
      .catch(err=>{
        if(err&&err.name==="AbortError")return;
        if(GS_Q!==q)return;
        GS_PENDING=false;
        document.getElementById("chatsearch-list").innerHTML=
          '<div class="empty"><i class="ti ti-cloud-off"></i> Couldn\'t reach the search service — your search is kept, try again.</div>';
      });
  },160);
}

// Client-side legs: rooms, people, projects — in-memory, instant.
function gsClientHits(q){
  const n=q.toLowerCase(),hit=[];
  for(const c of CHANNELS){
    if((c.name||"").toLowerCase().includes(n)||(c.topic||"").toLowerCase().includes(n))
      hit.push({kind:"channel",id:c.id,ref:"",label:"#"+chanDisplay(c),
        sub:c.topic||((c.kind==="public")?"public channel":"private channel"),snippet:""});
  }
  for(const m of (MEMBERS||[])){
    const u=m.username||m.name||"";
    if(u.toLowerCase().includes(n))hit.push({kind:"person",id:u,ref:"",label:u,sub:m.role||"",snippet:""});
  }
  for(const p of (PROJECTS||[])){
    if(`${p.id} ${p.name||""}`.toLowerCase().includes(n))
      hit.push({kind:"project",id:p.id,ref:"",label:p.name||p.id,
        sub:`${p.tickets||0} tickets · v${p.version||"0.0.0"}`,snippet:""});
  }
  return hit;
}

function gsRender(){
  const list=document.getElementById("chatsearch-list");
  // Read the LIVE input, not only the cached query: any render path that
  // fires while the cache is stale (a missed input event, a scope-chip
  // click racing the debounce) used to show "type at least 2 characters"
  // over a fully typed query (CXA-B170). Self-heal by kicking the search.
  const inp=document.getElementById("chatsearch-input");
  const liveQ=inp?inp.value.trim():GS_Q;
  if(liveQ!==GS_Q){chatSearchRun();return;}
  const q=GS_Q;
  // AC4: an explicit empty state for short queries — never a blank panel.
  if(q.length<2){
    list.innerHTML='<div class="empty">Type at least 2 characters — searching tickets, wiki and chat.</div>';
    return;
  }
  const client=gsClientHits(q);
  const srv=k=>GS_HITS.filter(h=>h.kind===k);
  const want=k=>GS_KIND==="all"||GS_KIND===k;
  // Grouped sections (the design mock's TICKETS / WIKI / CHAT); comments are
  // ticket threads, so they read under CHAT. Client legs lead.
  const sec=(key,title,hits)=>{
    if(!hits.length)return"";
    const shown=GS_EXPAND.has(key)?hits:hits.slice(0,GS_PAGE);
    const head=`<div class="cmdk-cat">${title} · ${hits.length}</div>`;
    const body=shown.map(h=>gsRow(h)).join("");
    const more=(hits.length>shown.length)
      ?`<div class="cs-more" onclick="gsMore('${key}')">Show ${hits.length-shown.length} more</div>`:"";
    return head+body+more;
  };
  let html="";
  if(want("chat"))html+=sec("rooms","PEOPLE & ROOMS",client.filter(h=>h.kind!=="project"));
  if(want("chat"))html+=sec("spaces","SPACES",client.filter(h=>h.kind==="project"));
  if(want("ticket"))html+=sec("ticket","TICKETS",srv("ticket"));
  if(want("page"))html+=sec("page","WIKI",srv("page"));
  if(want("chat"))html+=sec("chat","CHAT",srv("comment").concat(srv("message")));
  if(!html){
    list.innerHTML=GS_PENDING
      ?'<div class="empty"><i class="ti ti-loader-2 att-spin"></i> Searching tickets, wiki and chat…</div>'
      :`<div class="empty">No results for “${esc(q)}” — check the spelling or try fewer words.</div>`;
    return;
  }
  list.innerHTML=html;
  gsPaintSel();
}
function gsMore(key){GS_EXPAND.add(key);gsRender();}

function gsRow(h){
  const icon=GS_ICON[h.kind]||"search";
  // Ticket rows read "ID · title" like the board; thread rows name their ticket.
  const label=h.kind==="ticket"?`${h.id} · ${h.label}`
    :(h.kind==="comment"?`${h.ref} · ${h.label}`:h.label);
  const kindLabel=h.kind==="comment"?"thread":h.kind;
  const sub=h.sub?`<span>${esc(h.sub)}</span>`:"";
  const snip=h.snippet?`<span>${esc(h.snippet)}</span>`:"";
  return `<div class="cs-item" data-kind="${gsAttr(h.kind)}" data-id="${gsAttr(h.id)}" data-ref="${gsAttr(h.ref||"")}" data-link="${gsAttr(h.link||"")}" onclick="csOpen(this)">
    <i class="ti ti-${icon}"></i>
    <div class="cs-txt"><b>${esc(label)}</b>${sub}${snip}</div>
    <span class="cs-kind">${kindLabel}</span></div>`;
}
function gsPaintSel(){
  const rows=document.querySelectorAll("#chatsearch-list .cs-item");
  rows.forEach((r,i)=>r.classList.toggle("sel",i===GS_SEL));
  if(GS_SEL>=0&&rows[GS_SEL])rows[GS_SEL].scrollIntoView({block:"nearest"});
}

// Deep-link dispatch. Actions come from data attributes, never from composed
// strings — server-fed text can never end up inside inline JS.
function csOpen(el){
  closeChatSearch();
  const k=el.dataset.kind,id=el.dataset.id||"",ref=el.dataset.ref||"";
  if(k==="ticket")showTicket(id);
  else if(k==="comment")showTicket(ref||id);
  else if(k==="page")openWikiPage(id);
  else if(k==="message"){
    // Chat is a MODE, not a hash view — the shipped entry is focusChannel
    // (setMode("chat") + selectChannel); gotoMsg then anchors the message.
    focusChannel(ref||"general");
    gotoMsg(id,ref||"general");
  }
  else if(k==="channel")selectChannel(id);
  else if(k==="person")openDM(id);
  else if(k==="project")switchProject(id);
  else if(el.dataset.link)nav(el.dataset.link.replace(/^#/,""));
}

// Deep-link a wiki hit: nav('docs') kicks loadDocs, which honours a preset
// DOC_CUR, so selecting first makes the right page render when the list lands.
function openWikiPage(id){
  if(typeof docsWsClose==="function")docsWsClose();
  DOC_CUR=id;DOC_EDIT=false;
  nav("docs");
  if(DOCS.length){renderDocsList();renderDocMain();}
}

// Palette keybinding + keyboard-first navigation (↑ ↓ move, ↵ open, esc close).
document.addEventListener("keydown",e=>{
  if((e.metaKey||e.ctrlKey)&&e.key.toLowerCase()==="k"){e.preventDefault();openGlobalSearch();return;}
  const ov=document.getElementById("ov-chatsearch");
  if(!ov||!ov.classList.contains("open"))return;
  if(e.key==="Escape")closeChatSearch();
  else if(e.key==="ArrowDown"||e.key==="ArrowUp"){
    e.preventDefault();
    const n=document.querySelectorAll("#chatsearch-list .cs-item").length;
    if(!n)return;
    GS_SEL=e.key==="ArrowDown"?Math.min(GS_SEL+1,n-1):Math.max(GS_SEL-1,0);
    gsPaintSel();
  }else if(e.key==="Enter"&&GS_SEL>=0){
    const el=document.querySelectorAll("#chatsearch-list .cs-item")[GS_SEL];
    if(el)csOpen(el);
  }
});
