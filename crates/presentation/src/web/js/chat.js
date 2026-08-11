// Chat: channels, DMs, search, reactions, uploads, meetings ring.
// Split from index.html — classic script, load order matters (one shared scope).
let CHAT=[], CHATWS=null, CHATWS_PID=null, chatwsRetry=null, CHAT_LOAD_ERR=false;
let CHANNELS=[], CURCHAN="general";
// Initialise the always-visible chat dock: apply its collapsed state, load
// channels, open the current one, and connect the live socket.
async function openChat(){
  applyMode();
  loadMembers();
  loadProfiles();
  loadMeetings();
  await loadChannels();
  selectChannel(CURCHAN);
  openChatWS();
}
async function loadChannels(){
  try{CHANNELS=await(await fetch("/api/chat/channels")).json();}catch(e){CHANNELS=[{id:"general",name:"general",owner:""}];}
  if(!CHANNELS.some(c=>c.id===CURCHAN))CURCHAN="general";
  renderChannels();
}
// Rail sections fold and remember — a long DM list should never bury the
// meetings below it.
const RAIL_FOLD=JSON.parse(localStorage.getItem("coxrailfold")||"{}");
function toggleRailSection(key){
  RAIL_FOLD[key]=!RAIL_FOLD[key];
  localStorage.setItem("coxrailfold",JSON.stringify(RAIL_FOLD));
  applyRailFold();
}
function applyRailFold(){
  const map={chan:["chev-chan","chan-list"],dm:["chev-dm","dm-list"],meet:["chev-meet","meet-upnext"]};
  for(const k in map){
    const [chev,body]=map[k];
    const c=document.getElementById(chev),b=document.getElementById(body);
    if(!c||!b)continue;
    const folded=!!RAIL_FOLD[k];
    b.style.display=folded?"none":"";
    c.parentElement.classList.toggle("folded",folded);
  }
}
function railCount(id,n){const el=document.getElementById(id);if(el)el.textContent=n||"";}

// Pins are a personal ordering, kept on this device — a shared "pin" would
// be one person rearranging everyone's rail.
function railPins(){try{return new Set(JSON.parse(localStorage.getItem("coxpins")||"[]"));}catch(e){return new Set();}}
function togglePin_(id){
  const p=railPins();
  if(p.has(id))p.delete(id);else p.add(id);
  localStorage.setItem("coxpins",JSON.stringify([...p]));
  renderChannels();renderDMList();
}

function renderChannels(){
  const box=document.getElementById("chan-list");if(!box)return;
  // DMs live in their own section; everything else renders as a TREE so a
  // project's rooms read as belonging to it instead of four look-alike rows
  // called "agents" and "approvals".
  const all=CHANNELS.filter(c=>(c.kind||"")!=="dm");
  const parentOf=c=>c.parent||"";
  const kids=id=>all.filter(c=>parentOf(c)===id);
  const roots=all.filter(c=>!parentOf(c));
  const row=(c,depth)=>{
    const kind=c.kind||(c.id==="general"?"general":"private");
    const u=UNREAD[c.id]||0;
    // A sub-channel is a ROOM, not a folder — only the project root gets the
    // folder glyph, its children read as ordinary channels.
    const icon=kind==="private"?"lock":(kind==="project"&&!depth?"folder":"hash");
    const pad=8+depth*14;
    const children=kids(c.id);
    const label=depth?esc(c.name||c.id):esc(chanDisplay(c));
    return `<div class="chanitem${c.id===CURCHAN?' on':''}${u?' unread':''}${depth?' subchan':''}" role="button" tabindex="0"
        style="padding-left:${pad}px" aria-label="Channel ${label}${u?', '+u+' unread':''}"
        onkeydown="rowKey(event)" onclick="selectChannel('${esc(c.id)}')">
      ${depth?'<span class="subline"></span>':''}<i class="ti ti-${icon}"></i><span class="channm">${label}</span>${u?`<span class="chanbadge">${u>99?'99+':u}</span>`:''}
      <button class="chansub" title="New sub-channel here" onclick="event.stopPropagation();createChannel('${esc(c.id)}')"><i class="ti ti-plus"></i></button>
      <button class="chansub" title="${pin.has(c.id)?'Unpin':'Pin to top'}" onclick="event.stopPropagation();togglePin_('${esc(c.id)}')"><i class="ti ti-pin${pin.has(c.id)?'-filled':''}"></i></button>
      <button class="chancog" title="Channel settings" onclick="event.stopPropagation();openChannelSettings('${esc(c.id)}')"><i class="ti ti-settings"></i></button></div>`
      + children.map(k=>row(k,depth+1)).join("");
  };
  // Order: #general (the room everyone shares), then anything the user
  // pinned, then project rooms (where the work is), then the rest.
  const pin=railPins();
  const rank=c=>c.id==="general"?0:(pin.has(c.id)?1:(c.kind==="project"?2:3));
  const order=[...roots].sort((a,b)=>rank(a)-rank(b));
  box.innerHTML=order.map(c=>row(c,0)).join("");
  railCount("count-chan",all.length);
  applyRailFold();
  updateChannelBell();
}
// Keyboard activation for role="button" list rows (channels, DMs).
function rowKey(e){if(e.key==="Enter"||e.key===" "){e.preventDefault();e.currentTarget.click();}}
// Jump to a message, switching channels first when the hit is elsewhere. The
// target only exists after the new history renders, so poll briefly for it.
function gotoMsg(id,channel){
  closeThread();
  const scroll=()=>{const el=document.getElementById("msg-"+id);if(!el)return false;
    el.scrollIntoView({behavior:"smooth",block:"center"});
    el.classList.add("msg-flash");setTimeout(()=>el.classList.remove("msg-flash"),1600);return true;};
  if(channel&&channel!==CURCHAN){selectChannel(channel);let n=0;
    const t=setInterval(()=>{if(scroll()||++n>20)clearInterval(t);},100);}
  else scroll();
}
function getActiveChannel(){return CURCHAN||"general";}
function currentChannel(){return CHANNELS.find(c=>c.id===CURCHAN)||{id:"general",name:"general",owner:""};}
function selectChannel(id){
  const switching=id!==CURCHAN;
  CURCHAN=id;
  clearUnread(id);
  const ch=currentChannel();
  const kind=ch.kind||(id==="general"?"general":"private");
  const prefix=kind==="dm"?"@ ":kind==="private"?"🔒 ":"# ";
  document.getElementById("chat-title").textContent=prefix+chanDisplay(ch);
  const sub=document.getElementById("chat-sub");
  const n=(ch.members||[]).length;
  sub.textContent = kind==="general" ? "everyone in the workspace"
    : kind==="dm" ? "direct message"
    : kind==="project" ? `project channel · ${n} member${n===1?'':'s'}`
    : `private channel · ${n} member${n===1?'':'s'}`;
  const mc=document.getElementById("chat-memcount");
  if(mc)mc.textContent=kind==="general"?"Members":(n+" member"+(n===1?"":"s"));
  // Call buttons only for 1:1 DMs.
  const dm=kind==="dm";
  document.getElementById("chat-call").style.display=dm?"inline-flex":"none";
  document.getElementById("chat-vcall").style.display=dm?"inline-flex":"none";
  document.getElementById("chat-hooks").style.display=dm?"none":"inline-flex";
  updateChannelBell();
  updateOnline();
  // Only private channels take invites; project membership follows assignment.
  const me=(ME&&ME.username)||"user";
  const canInvite=kind==="private"&&(ch.owner===me||(ch.inviters||[]).includes(me));
  document.getElementById("chat-invite").style.display=canInvite?"inline-flex":"none";
  renderChannels();renderDMList();
  // Show a spinner immediately on a real switch so the previous channel's
  // messages don't linger while the new history loads.
  if(switching){
    const box=document.getElementById("chat-msgs");
    if(box){box.innerHTML='<div class="chatempty"><i class="ti ti-loader-2 att-spin"></i><div>Loading…</div></div>';box.dataset.sig="loading";}
    CHAT=[];CHAT_LOAD_ERR=false;
  }
  loadChatHistory();
}
async function loadChatHistory(){
  const chan=CURCHAN;
  try{
    const r=await fetch("/api/chat/messages?channel="+encodeURIComponent(chan));
    if(!r.ok)throw new Error(r.status);
    const data=await r.json();
    if(chan!==CURCHAN)return; // channel switched mid-flight
    CHAT=data;CHAT_LOAD_ERR=false;
  }catch(e){
    if(chan!==CURCHAN)return;
    // Distinguish a failed load from a genuinely empty room (see renderChatList):
    // clear so we never bleed another channel's messages, and flag the error.
    CHAT=[];CHAT_LOAD_ERR=true;
  }
  CHAT.forEach(markSeen); // history is not "new" — don't notify for it
  renderChatList(true);
}
// `parent` is set when the + on a channel row was used: the new room is opened
// inside that one and starts with its members.
async function createChannel(parent){
  const where=parent?` inside #${chanDisplay(CHANNELS.find(c=>c.id===parent)||{name:parent})}`:"";
  const name=await coxModal({title:parent?"New sub-channel":"New channel",
    message:`Name the channel${where}. It is private (invite-only) unless you switch it to public.`,
    input:{placeholder:"e.g. design-review"},
    toggle:{label:"Public — anyone in the workspace can join",value:false},
    confirmText:"Create"});
  if(!name||!name.trim&&!name.value)return;
  const n=(typeof name==="string"?name:name.value||"").trim();
  if(!n)return;
  const kind=(typeof name==="object"&&name.toggle)?"public":"private";
  try{
    const r=await fetch("/api/chat/channels",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({name:n,kind,parent:parent||null})});
    if(!r.ok){toasty(await r.text()||"could not create channel","err");return;}
    const ch=await r.json();
    await loadChannels();
    selectChannel(ch.id);
    toasty("Channel #"+ch.name+" created ("+kind+")","ok");
  }catch(e){toasty("could not create channel","err");}
}
async function inviteToChannel(){
  const who=await coxModal({title:"Invite to channel",message:"Username cần mời. Tip: thêm \" +invite\" để họ cũng được quyền mời người khác.",input:{placeholder:"username  (+invite)"},confirmText:"Invite"});
  if(!who||!who.trim())return;
  let user=who.trim(),delegate=false;
  if(/\+invite\s*$/i.test(user)){delegate=true;user=user.replace(/\+invite\s*$/i,"").trim();}
  try{
    const r=await fetch("/api/chat/channels/"+encodeURIComponent(CURCHAN)+"/invite",
      {method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({user,delegate})});
    if(!r.ok){toasty(await r.text()||"could not invite","err");return;}
    await loadChannels();selectChannel(CURCHAN);
    toasty((delegate?"Delegated invite to ":"Invited ")+user,"ok");
  }catch(e){toasty("could not invite","err");}
}
// ── Chat notifications: unread badges, desktop notifications, title flash ────
// Notifications work app-wide (any page), driven by the always-on chat
// WebSocket plus the 1s SSE snapshot. SEEN dedups across both transports so a
// message notifies exactly once; chatPrimed suppresses notifications for the
// history that already existed when we connected.
let UNREAD={}; // per-channel unread counts
let SEEN=new Set(), chatPrimed=false;
// Sidebar mode: "workspace" (menu + views) or "chat" (channels + conversation).
let MODE=(localStorage.getItem("cox_mode")==="chat")?"chat":"workspace";
function isChatMode(){return MODE==="chat";}
function isManageMode(){return MODE==="manage";}
function markSeen(m){SEEN.add(chatKey(m));}
// ── Channel settings: privacy, who may invite, members ──────────────────────
// Everything a room's owner decides about their room, in one place. Admins
// outrank ownership — the server enforces that; this only shows the controls.
async function openChannelSettings(id){
  const ch=CHANNELS.find(c=>c.id===id);
  if(!ch){toasty("channel not found","err");return;}
  CHSET={id,tab:"general"};
  document.getElementById("chset-title").textContent="#"+chanDisplay(ch);
  renderChannelSettings();
  document.getElementById("ov-chset").classList.add("open");
}
let CHSET=null;
function chsetTab(t){if(CHSET){CHSET.tab=t;renderChannelSettings();}}
function renderChannelSettings(){
  if(!CHSET)return;
  const ch=CHANNELS.find(c=>c.id===CHSET.id);if(!ch)return;
  const isGeneral=ch.id==="general";
  for(const t of ["general","members","permissions"]){
    const b=document.getElementById("chset-tab-"+t);
    if(b)b.classList.toggle("on",CHSET.tab===t);
  }
  const box=document.getElementById("chset-body");
  if(CHSET.tab==="general"){
    box.innerHTML=`
      <label class="coxmodal-toggle">
        <input type="checkbox" id="chset-public" ${(ch.kind==="public")?"checked":""} ${isGeneral?"disabled":""}
          onchange="saveChannelSettings({kind:this.checked?'public':'private'})">
        <span class="cmsw"></span>
        <span>Public — anyone in the workspace can join and read</span>
      </label>
      ${isGeneral?'<div class="msub">#general is always open: a team needs one room nobody can be shut out of.</div>':''}
      <div class="fr" style="margin-top:14px"><span class="lbl">Topic</span>
        <input id="chset-topic" value="${esc(ch.topic||"")}" placeholder="what this channel is for" style="flex:1"
          onchange="saveChannelSettings({topic:this.value})"></div>
      ${(ch.id==="general"||ch.id==="agents")?"":`
      <div style="margin-top:18px;padding-top:14px;border-top:1px solid var(--border)">
        <div style="font-size:12px;color:var(--muted);margin-bottom:8px">Deleting removes the channel and its sub-channels for everyone.</div>
        <button class="tk-btn danger" onclick="deleteChannel('${esc(ch.id)}')"><i class="ti ti-trash"></i> Delete channel</button>
      </div>`}`;
  } else if(CHSET.tab==="permissions"){
    box.innerHTML=`
      <label class="coxmodal-toggle">
        <input type="checkbox" id="chset-openinv" ${ch.open_invite?"checked":""}
          onchange="saveChannelSettings({open_invite:this.checked})">
        <span class="cmsw"></span>
        <span>Any member may invite others</span>
      </label>
      <div class="msub">With this off, only the owner and the people they name below can add or remove members. Admins can always do both.</div>
      <div style="margin-top:14px;font-size:11px;font-weight:700;letter-spacing:.04em;text-transform:uppercase;color:var(--dim)">Can invite &amp; remove</div>
      <div id="chset-inviters">${(ch.members||[]).map(m=>`
        <label class="chset-row"><input type="checkbox" ${(ch.inviters||[]).includes(m)?"checked":""}
          onchange="delegateInvite('${esc(m)}',this.checked)"> ${esc(m)}</label>`).join("")||'<div class="empty">no members yet</div>'}</div>`;
  } else {
    box.innerHTML=`
      <div id="chset-members">${(ch.members||[]).map(m=>`
        <div class="chset-row"><span>${esc(m)}${m===ch.owner?' <span class="pbadge on">owner</span>':''}</span>
          ${m===ch.owner?'':`<button class="btn-ghost" onclick="kickMember('${esc(m)}')">Remove</button>`}</div>`).join("")||'<div class="empty">no members yet</div>'}</div>
      <button class="save pf-btn" style="margin-top:12px" onclick="inviteToSettingsChannel()"><i class="ti ti-user-plus"></i> Invite someone</button>`;
  }
}
async function deleteChannel(id){
  const ok=await coxModal({title:"Delete #"+id,message:"Xoá channel này và mọi sub-channel của nó? Không hoàn tác được.",confirmText:"Delete"});
  if(!ok)return;
  try{
    const r=await fetch("/api/chat/channels/"+encodeURIComponent(id),{method:"DELETE"});
    if(!r.ok){toasty(await r.text(),"err");return;}
    close_("ov-chset");CHSET=null;
    if(CURCHAN===id)selectChannel("general");
    await loadChannels();toasty("Channel deleted","ok");
  }catch(e){toasty("Delete failed","err");}
}

async function saveChannelSettings(patch){
  if(!CHSET)return;
  try{
    // Channels live in the system-chat store (they are created through
    // /api/chat/channels), so their settings do too. Pointing this at the
    // per-project router answered "no such project" on every save.
    const r=await fetch(`/api/chat/channels/${encodeURIComponent(CHSET.id)}/settings`,
      {method:"PATCH",headers:{"Content-Type":"application/json"},body:JSON.stringify(patch)});
    if(!r.ok){toasty(await r.text()||"could not save","err");return;}
    await loadChannels();renderChannelSettings();toasty("Saved","ok");
  }catch(e){toasty("could not save","err");}
}
async function kickMember(user){
  if(!CHSET)return;
  const ok=await coxModal({title:"Remove member",message:`Remove ${user} from this channel?`,confirmText:"Remove",danger:true});
  if(!ok)return;
  try{
    const r=await fetch(`/api/chat/channels/${encodeURIComponent(CHSET.id)}/members/${encodeURIComponent(user)}`,{method:"DELETE"});
    if(!r.ok){toasty(await r.text()||"could not remove","err");return;}
    await loadChannels();renderChannelSettings();
  }catch(e){toasty("could not remove","err");}
}
async function delegateInvite(user,on){
  try{
    await fetch(`/api/chat/channels/${encodeURIComponent(CHSET.id)}/invite`,
      {method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({user,delegate:on})});
    await loadChannels();renderChannelSettings();
  }catch(e){toasty("could not update","err");}
}
// Invite from the Channel Settings modal — targets the channel whose settings
// are open (CHSET.id), via the project-scoped route the rest of that modal uses.
async function inviteToSettingsChannel(){
  if(!CHSET){toasty("open channel settings first","err");return;}
  const who=await coxModal({title:"Invite to channel",input:{placeholder:"username"},confirmText:"Invite"});
  const u=(typeof who==="string"?who:"").trim();if(!u)return;
  try{
    const r=await fetch(`/api/chat/channels/${encodeURIComponent(CHSET.id)}/invite`,
      {method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({user:u})});
    if(!r.ok){toasty(await r.text()||"could not invite","err");return;}
    await loadChannels();renderChannelSettings();
  }catch(e){toasty("could not invite","err");}
}

// ── Chat search: channels, people, messages, in one palette ─────────────────
function openChatSearch(){
  const ov=document.getElementById("ov-chatsearch");
  document.getElementById("chatsearch-input").value="";
  const sc=searchScope();
  document.getElementById("chatsearch-input").placeholder=`Search ${sc.label}…`;
  document.getElementById("chatsearch-list").innerHTML=`<div class="empty">Type to search ${sc.label}.</div>`;
  ov.classList.add("open");
  setTimeout(()=>document.getElementById("chatsearch-input").focus(),40);
}
// The rail's single search. In chat it looks at rooms, people and messages;
// everywhere else it looks at the work — tickets and wiki pages — so the same
// box is useful in all three modes instead of being a chat-only feature that
// happens to sit above the switch.
function openGlobalSearch(){ openChatSearch(); }

// What the palette searches depends on where you are: rooms and messages in
// Chat, work in Space, people and projects in Manage. One box, the answers of
// the room you are standing in.
function searchScope(){
  if(MODE==="chat")return {kinds:["channel","person","message"],label:"channels, people, messages"};
  if(MODE==="manage")return {kinds:["person","project"],label:"people and projects"};
  return {kinds:["ticket","page","person"],label:"tickets, pages, people"};
}

function searchWorkItems(q){
  const hit=[];
  const s=STATE||{};
  for(const t of (s.tickets||[])){
    const hay=`${t.id} ${t.title} ${t.description||""}`.toLowerCase();
    if(hay.includes(q)) hit.push({t:"ticket",label:`${t.id} · ${t.title}`,sub:t.status,act:`showTicket('${esc(t.id)}')`});
    if(hit.length>=12)break;
  }
  for(const d of (s.docs||[])){
    if(`${d.title} ${d.body||""}`.toLowerCase().includes(q))
      hit.push({t:"page",label:d.title,sub:d.folder||"wiki",act:`nav('docs')`});
    if(hit.length>=20)break;
  }
  return hit;
}

function chatSearchRun(){
  const q=document.getElementById("chatsearch-input").value.trim().toLowerCase();
  const list=document.getElementById("chatsearch-list");
  const scope=searchScope();
  if(!q){list.innerHTML=`<div class="empty">Type to search ${scope.label}.</div>`;return;}
  let hit=[];
  for(const c of CHANNELS){
    if((c.name||"").toLowerCase().includes(q)||(c.topic||"").toLowerCase().includes(q))
      hit.push({t:"channel",label:"#"+chanDisplay(c),sub:c.topic||((c.kind==="public")?"public channel":"private channel"),act:`selectChannel('${esc(c.id)}')`});
  }
  for(const m of (MEMBERS||[])){
    const n=m.username||m.name||"";
    if(n.toLowerCase().includes(q)) hit.push({t:"person",label:n,sub:m.role||"",act:`openDM('${esc(n)}')`});
  }
  for(const h of searchWorkItems(q)) hit.push(h);
  for(const p of (PROJECTS||[])){
    if(`${p.id} ${p.name||""}`.toLowerCase().includes(q))
      hit.push({t:"project",label:p.name||p.id,sub:`${p.tickets||0} tickets · v${p.version||"0.0.0"}`,act:`switchProject('${esc(p.id)}')`});
  }
  for(const msg of (CHAT||[]).slice(-800).reverse()){
    const b=(msg.body||"");
    if(b.toLowerCase().includes(q)){
      hit.push({t:"message",label:b.slice(0,90),sub:`${msg.user||""} · #${msg.channel||"general"}`,act:`selectChannel('${esc(msg.channel||"general")}')`});
      if(hit.filter(h=>h.t==="message").length>=12)break;
    }
  }
  const icon={channel:"hash",person:"user",message:"message-2",ticket:"ticket",page:"file-text",project:"folder"};
  // Keep only what this tab is about; everything else is noise here.
  hit=hit.filter(h=>scope.kinds.includes(h.t));
  list.innerHTML=hit.length?hit.slice(0,30).map(h=>`
    <div class="cs-item" onclick="closeChatSearch();${h.act}">
      <i class="ti ti-${icon[h.t]}"></i>
      <div class="cs-txt"><b>${esc(h.label)}</b>${h.sub?`<span>${esc(h.sub)}</span>`:''}</div>
      <span class="cs-kind">${h.t}</span></div>`).join("")
    :'<div class="empty">Nothing matched.</div>';
}
function closeChatSearch(){document.getElementById("ov-chatsearch").classList.remove("open");}
document.addEventListener("keydown",e=>{
  if((e.metaKey||e.ctrlKey)&&e.key.toLowerCase()==="k"){e.preventDefault();openGlobalSearch();}
});

// Enterprise modal replacing browser prompt()/confirm(): returns a Promise —
// resolves the input string (or true) on confirm, null on cancel.
let _cmResolve=null;
function coxModal(o){
  return new Promise(res=>{
    _cmResolve=res;
    const m=document.getElementById("cox-modal");
    document.getElementById("cm-title").textContent=o.title||"Confirm";
    document.getElementById("cm-msg").textContent=o.message||"";
    const inp=document.getElementById("cm-input");
    inp.hidden=!o.input; inp.value=(o.input&&o.input.value)||""; inp.placeholder=(o.input&&o.input.placeholder)||"";
    // Optional switch (e.g. public/private) — resolves alongside the text.
    const tg=document.getElementById("cm-toggle");
    if(tg){tg.hidden=!o.toggle;
      if(o.toggle){document.getElementById("cm-toggle-label").textContent=o.toggle.label||"";
        document.getElementById("cm-toggle-input").checked=!!o.toggle.value;}}
    document.getElementById("cm-ok").textContent=o.confirmText||"OK";
    document.getElementById("cm-cancel").textContent=o.cancelText||"Cancel";
    m.querySelector(".coxmodal-card").classList.toggle("danger",!!o.danger);
    m.hidden=false;
    if(o.input)setTimeout(()=>inp.focus(),30);
    m.onkeydown=e=>{if(e.key==="Escape")coxModalCancel();if(e.key==="Enter"&&(e.metaKey||!o.input||!(o.input.multiline)))coxModalOk();};
    m.tabIndex=-1;m.focus();
  });
}
function coxModalOk(){
  const m=document.getElementById("cox-modal");const inp=document.getElementById("cm-input");
  const tg=document.getElementById("cm-toggle");
  const val=inp.hidden?true:inp.value.trim();
  const out=(tg&&!tg.hidden)?{value:inp.hidden?"":inp.value.trim(),toggle:document.getElementById("cm-toggle-input").checked}:val;
  m.hidden=true; if(_cmResolve){_cmResolve(out);_cmResolve=null;}
}
function coxModalCancel(){
  const m=document.getElementById("cox-modal");m.hidden=true;
  if(_cmResolve){_cmResolve(null);_cmResolve=null;}
}
function updateSegments(){
  const mg=document.getElementById("mode-manage");
  if(mg)mg.style.display=(ME&&ME.role==="super")?"":"none";
  if(mg)mg.classList.toggle("on",MODE==="manage");
  const ws=document.getElementById("mode-ws");
  if(ws)ws.classList.toggle("on",MODE==="workspace");
  const ch=document.getElementById("mode-chat");
  if(ch)ch.classList.toggle("on",MODE==="chat");
}
function setMode(m){
  MODE=(m==="chat")?"chat":(m==="manage")?"manage":"workspace";
  localStorage.setItem("cox_mode",MODE);
  applyMode();
}
function applyMode(){
  const chat=isChatMode(),manage=MODE==="manage";
  document.body.classList.toggle("mode-chat",chat);
  document.body.classList.toggle("mode-manage",manage);
  if(chat){renderChannels();selectChannel(CURCHAN);clearUnread(CURCHAN);
    setTimeout(()=>{const i=document.getElementById("chat-input");if(i)i.focus();},40);}
  // Entering Manage lands on Spaces; leaving it returns to the workspace views.
  if(manage&&!String(CUR).startsWith("mg-"))nav("mg-spaces");
  if(!manage&&String(CUR).startsWith("mg-"))nav("overview");
  updateModeBadge();
  try{updateSegments();}catch(e){}
}
// Notification click / native bridge: jump into Chat mode on that channel.
function focusChannel(id){setMode("chat");selectChannel(id);}
// Unread count on the "Chat" segment button (shown while in Workspace mode).
function updateModeBadge(){
  const b=document.getElementById("mode-chat-badge");if(!b)return;
  const n=totalUnread();
  b.textContent=n>99?"99+":n;b.style.display=(!isChatMode()&&n>0)?"":"none";
}
// Central handler for every incoming chat message (live socket or fallback).
function onChatIncoming(m){
  if(m.op==="typing"){if(m.user&&m.user!==ME?.username){TYPING.set(m.user,Date.now());renderTyping();}return;}
  if(m.op==="status"){/* status broadcast handled elsewhere */return;}
  if(m.op==="edit"){const idx=CHAT.findIndex(x=>x.id===m.msg.id);if(idx>=0){CHAT[idx].body=m.msg.body;CHAT[idx].edited=m.msg.edited;renderChatList(true);}return;}
  if(m.op==="delete"){const idx=CHAT.findIndex(x=>x.id===m.msg.id);if(idx>=0){CHAT[idx].deleted=true;CHAT[idx].body="";renderChatList(true);}return;}
  if(m.op==="pin"){loadPins();return;}
  const key=chatKey(m);
  const chan=m.channel||"general";
  const me=(ME&&ME.username)||"user";
  // Render into the open conversation if this message belongs there.
  if(isChatMode()&&chan===CURCHAN&&!chatHas(m)){
    CHAT.push(m);CHAT.sort((a,b)=>(a.at||"").localeCompare(b.at||""));renderChatList();
  }
  if(SEEN.has(key))return;
  SEEN.add(key);
  // "Actively watching" means the chat dock is open on this channel AND the app
  // is the focused window. hasFocus() (not just !hidden) distinguishes a
  // background-but-visible window — otherwise notifications never fire when the
  // app sits behind another window.
  const active=(isChatMode()&&chan===CURCHAN&&!document.hidden&&document.hasFocus());
  if(m.user!==me&&!active){
    UNREAD[chan]=(UNREAD[chan]||0)+1;
    renderChannels();updateModeBadge();
    updateTitle();
    notifyMessage(chan,m);
  }
}
// Process the #general messages carried on each SSE snapshot (the fallback path
// and the app-wide notification source while off the Chat view). Primes silently
// on the first snapshot so pre-existing history doesn't fire notifications.
function ingestChatSnapshot(list){
  if(!Array.isArray(list))return;
  if(!chatPrimed){list.forEach(markSeen);chatPrimed=true;return;}
  list.forEach(m=>{const c=m.channel||"general";if(c==="general"||c==="agents")onChatIncoming(m);});
}
// Start the live chat connection as soon as a project is active, so
// notifications arrive on any page — not only while the Chat view is open.
function initChatBackground(){
  SEEN=new Set();UNREAD={};chatPrimed=false;
  openChat();
}
// Workspace members (for @mentions, member list, DMs).
let MEMBERS=[];
async function loadMembers(){try{MEMBERS=await(await fetch("/api/chat/members")).json();}catch(e){MEMBERS=[];}renderDMList();}
function memberName(u){const m=MEMBERS.find(x=>x.username===u);return (m&&m.name&&m.name.trim())?m.name:u;}
// Slack-style direct-messages list in the sidebar: every teammate, online dot,
// click to open a 1:1 DM. The active DM is highlighted.
function renderDMList(){
  const box=document.getElementById("dm-list");if(!box)return;
  const me=(ME&&ME.username)||"user";
  const si=document.getElementById("dm-search");const q=(si?si.value:"").toLowerCase();
  const online=new Set(ONLINE||[]);
  const cur=currentChannel();
  const curOther=(cur.kind==="dm")?(cur.members||[]).find(x=>x!==me):null;
  let users=(MEMBERS||[]).filter(u=>u.username!==me);
  if(q)users=users.filter(u=>u.username.toLowerCase().includes(q)||(u.name||"").toLowerCase().includes(q));
  // Dedupe by username: two accounts whose DISPLAY names differ only by case
  // ("choper" vs "Chopper") used to render as two identical-looking rows.
  const seen=new Set();
  users=users.filter(u=>{const k=(u.username||"").toLowerCase();if(seen.has(k))return false;seen.add(k);return true;});
  // Unread first, then online, then recency of the DM, then name.
  const dmUnread=u=>{const ch=(CHANNELS||[]).find(c=>c.kind==="dm"&&(c.members||[]).includes(u.username));return ch?(UNREAD[ch.id]||0):0;};
  const dmLast=u=>{const ch=(CHANNELS||[]).find(c=>c.kind==="dm"&&(c.members||[]).includes(u.username));return ch&&ch.last_at?Date.parse(ch.last_at)||0:0;};
  const pinned=railPins();
  const dmChanId=u=>{const ch=(CHANNELS||[]).find(c=>c.kind==="dm"&&(c.members||[]).includes(u.username));return ch?ch.id:"dm:"+u.username;};
  users.sort((a,b)=>(pinned.has(dmChanId(b))-pinned.has(dmChanId(a)))
    ||(dmUnread(b)>0)-(dmUnread(a)>0)
    ||(online.has(b.username)-online.has(a.username))
    ||(dmLast(b)-dmLast(a))
    ||(a.name||a.username).localeCompare(b.name||b.username));
  // A rail is not a directory: show the people you actually talk to.
  const RAIL_DM_MAX=8;
  const shown=users.slice(0,RAIL_DM_MAX);
  const hidden=users.length-shown.length;
  box.innerHTML=shown.length?shown.map(u=>{
    const on=online.has(u.username);const active=curOther===u.username;
    const p=PROFILES[u.username]||{};
    const av=p.avatar?`<span class="dmav" style="padding:0;overflow:hidden"><img src="${esc(p.avatar)}" style="width:100%;height:100%;object-fit:cover;border-radius:inherit"><span class="dmpres${on?' on':''}"></span></span>`
      :`<span class="dmav" style="background:${userColor(u.username)}">${esc((u.name||u.username).slice(0,2).toUpperCase())}<span class="dmpres${on?' on':''}"></span></span>`;
    const un=dmUnread(u);
    // Two people whose display names collide read as duplicates — show the
    // username underneath so the rail stays unambiguous.
    const dupName=shown.filter(x=>(x.name||x.username).toLowerCase()===(u.name||u.username).toLowerCase()).length>1;
    const sub=dupName?`<span style="font-size:10.5px;color:var(--dim);margin-left:4px">@${esc(u.username)}</span>`:"";
    return `<div class="chanitem dmitem${active?' on':''}${un?' unread':''}" role="button" tabindex="0" aria-label="Direct message ${esc(u.name||u.username)}${un?', '+un+' unread':''}" onkeydown="rowKey(event)" onclick="openDM('${esc(u.username)}')" title="@${esc(u.username)}${p.status_text?' · '+esc(p.status_text):''}">
      ${av}<span class="channm">${esc(u.name||u.username)}</span>${sub}${statusChip(u.username)}${un?`<span class="chanbadge">${un>99?'99+':un}</span>`:''}
      <button class="chansub" title="${pinned.has(dmChanId(u))?'Unpin':'Pin to top'}" onclick="event.stopPropagation();togglePin_('${esc(dmChanId(u))}')"><i class="ti ti-pin${pinned.has(dmChanId(u))?'-filled':''}"></i></button></div>`;
  }).join("")+(hidden>0?`<div class="chanitem" role="button" tabindex="0" style="color:var(--dim);font-size:12px" onclick="openChatSearch()">+${hidden} more — search people</div>`:""):'<div class="dm-empty">No teammates yet</div>';
  railCount("count-dm",users.length);
  applyRailFold();
}
// ── @mention autocomplete ───────────────────────────────────────────────────
let MENTION={open:false,items:[],sel:0,start:-1};
function mentionOnInput(inp){
  const val=inp.value,pos=inp.selectionStart;
  const m=val.slice(0,pos).match(/@([a-zA-Z0-9._-]*)$/);
  if(!m){closeMention();return;}
  const q=m[1].toLowerCase();
  const items=(MEMBERS||[]).filter(u=>u.username.toLowerCase().includes(q)||(u.name||"").toLowerCase().includes(q)).slice(0,8);
  if(!items.length){closeMention();return;}
  MENTION={open:true,items,sel:0,start:pos-m[0].length};
  renderMention();
}
function renderMention(){
  let pop=document.getElementById("mention-pop");
  if(!pop){pop=document.createElement("div");pop.id="mention-pop";pop.className="mentionpop";const c=document.querySelector("#chat-main .chatcompose");if(!c)return;c.appendChild(pop);}
  pop.innerHTML=MENTION.items.map((u,i)=>`<div class="mrow${i===MENTION.sel?' on':''}" onmousedown="event.preventDefault();pickMention(${i})"><span class="mav" style="background:${userColor(u.username)}">${esc((u.name||u.username).slice(0,2).toUpperCase())}</span><span><span class="mnm">${esc(u.name||u.username)}</span> <span class="mun">@${esc(u.username)}</span></span></div>`).join("");
  pop.style.display="block";
}
function closeMention(){MENTION.open=false;const p=document.getElementById("mention-pop");if(p)p.style.display="none";}
function pickMention(i){
  const inp=document.getElementById("chat-input");const u=MENTION.items[i];if(!u)return;
  const val=inp.value,pos=inp.selectionStart;
  inp.value=val.slice(0,MENTION.start)+"@"+u.username+" "+val.slice(pos);
  const np=MENTION.start+u.username.length+2;inp.setSelectionRange(np,np);
  closeMention();inp.focus();
}
// Vietnamese/CJK IME: the input method commits diacritics with a synthetic
// Enter (keyCode 229 / isComposing) — treating that as "send" sprays one
// sentence across several partial messages. Every Enter-to-send handler must
// pass through this guard.
function imeEnter(e){return e.isComposing||e.keyCode===229;}
function chatInputKey(e){
  if(imeEnter(e))return;
  if(MENTION.open){
    if(e.key==="ArrowDown"){e.preventDefault();MENTION.sel=(MENTION.sel+1)%MENTION.items.length;renderMention();return;}
    if(e.key==="ArrowUp"){e.preventDefault();MENTION.sel=(MENTION.sel-1+MENTION.items.length)%MENTION.items.length;renderMention();return;}
    if(e.key==="Enter"||e.key==="Tab"){e.preventDefault();pickMention(MENTION.sel);return;}
    if(e.key==="Escape"){e.preventDefault();closeMention();return;}
  }
  if(e.key==="Enter")sendChat();
}
// ── Members list + direct messages ──────────────────────────────────────────
function channelMembers(){
  const ch=currentChannel();const kind=ch.kind||"private";
  if(kind==="general")return (MEMBERS||[]).map(m=>m.username);
  return ch.members||[];
}
function openMembers(){
  document.getElementById("mem-search").value="";
  const ch=currentChannel();
  document.getElementById("mem-title").textContent="Members · "+((ch.kind==="dm")?"direct message":("#"+ch.name));
  renderMembersList();
  document.getElementById("ov-members").classList.add("open");
  setTimeout(()=>document.getElementById("mem-search").focus(),40);
}
function renderMembersList(){
  const q=(document.getElementById("mem-search").value||"").toLowerCase();
  const me=(ME&&ME.username)||"user";
  let users=channelMembers().map(u=>({username:u,name:memberName(u)}));
  // #general: search the whole workspace directory.
  if(q&&currentChannel().kind==="general")users=(MEMBERS||[]).map(m=>({username:m.username,name:m.name||m.username}));
  users=users.filter(u=>u.username.toLowerCase().includes(q)||(u.name||"").toLowerCase().includes(q));
  document.getElementById("mem-sub").textContent=users.length+" member"+(users.length===1?"":"s");
  const box=document.getElementById("mem-list");
  box.innerHTML=users.map(u=>`<div class="memrow2">
    <span class="memav" style="background:${userColor(u.username)}">${esc((u.name||u.username).slice(0,2).toUpperCase())}</span>
    <div class="memmeta"><div class="memnm">${esc(u.name||u.username)}${u.username===me?' <span class="memyou">you</span>':''}</div><div class="memun">@${esc(u.username)}</div></div>
    ${u.username===me?'':`<div class="memacts"><button class="memic" title="Voice call" onclick="close_('ov-members');startCall('${esc(u.username)}',false)"><i class="ti ti-phone"></i></button><button class="memic" title="Video call" onclick="close_('ov-members');startCall('${esc(u.username)}',true)"><i class="ti ti-video"></i></button><button class="memdm" onclick="openDM('${esc(u.username)}')"><i class="ti ti-message-2"></i> Message</button></div>`}
  </div>`).join("")||'<div class="empty">no members found</div>';
}
async function openDM(username){
  try{
    const r=await fetch("/api/chat/dm",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({user:username})});
    if(!r.ok){toasty(await r.text()||"could not open DM","err");return;}
    const ch=await r.json();
    close_("ov-members");
    await loadChannels();
    focusChannel(ch.id);
  }catch(e){toasty("could not open DM","err");}
}
// Display name for a channel: DMs show the other person, others show their name.
function chanDisplay(c){
  if((c.kind||"")!=="dm")return c.name;
  const me=(ME&&ME.username)||"user";
  const other=(c.members||[]).find(u=>u!==me)||c.name;
  return memberName(other);
}
// ── Voice / video calls (WebRTC, signaled over the chat WebSocket) ───────────
let CALL={pc:null,peer:null,local:null,video:false,incoming:null,status:""};
let ICE=null,ICE_AT=0;
// Fetch ICE servers (STUN + TURN with short-lived creds) from the server; cache
// briefly. Falls back to public STUN if the endpoint is unreachable.
async function iceConfig(){
  if(ICE&&Date.now()-ICE_AT<120000)return ICE;
  try{ICE=(await(await fetch("/api/chat/ice")).json()).iceServers;ICE_AT=Date.now();}
  catch(e){ICE=[{urls:"stun:stun.l.google.com:19302"}];}
  return ICE;
}
function sendSignal(to,kind,payload){
  if(CHATWS&&CHATWS.readyState===1)CHATWS.send(JSON.stringify({type:"signal",to,kind,payload}));
}
function newPC(iceServers){
  const pc=new RTCPeerConnection({iceServers:iceServers||[{urls:"stun:stun.l.google.com:19302"}]});
  CALL.pc=pc;
  pc.onicecandidate=e=>{if(e.candidate)sendSignal(CALL.peer,"ice",{candidate:e.candidate});};
  pc.ontrack=e=>{const v=document.getElementById("call-remote");if(v&&e.streams[0]){v.srcObject=e.streams[0];setCallStatus("connected");}};
  pc.onconnectionstatechange=()=>{if(["failed","disconnected","closed"].includes(pc.connectionState))endCall(false);};
  return pc;
}
// Why the call could not start, in words the person can act on. A toast that
// says "cannot access mic/camera" and disappears leaves them clicking a button
// that looks dead; a denied permission and an absent device need different
// fixes, and only the browser knows which happened.
function callDeviceError(err){
  const name=(err&&err.name)||"";
  let msg;
  if(!window.isSecureContext){
    msg="Calls need a secure page. Open CoXAgent over https or on localhost.";
  }else if(name==="NotAllowedError"||name==="SecurityError"){
    msg="Microphone/camera access was denied. Allow it for CoXAgent in System Settings → Privacy & Security, then try again.";
  }else if(name==="NotFoundError"||name==="OverconstrainedError"){
    msg="No microphone or camera found on this machine.";
  }else if(name==="NotReadableError"){
    msg="Another app is using the microphone/camera. Close it and try again.";
  }else{
    msg="Could not start the call: "+(name||err&&err.message||"unknown error");
  }
  // Persistent, not a toast: this needs a decision, not a glance.
  alert(msg);
}
async function startCall(peer,video){
  if(CALL.pc){toasty("Already in a call","err");return;}
  try{
    CALL.peer=peer;CALL.video=video;
    let stream;
    try{ stream=await navigator.mediaDevices.getUserMedia({audio:true,video}); }
    catch(err){
      // A video call that cannot get the camera is still a phone call. Try
      // audio before giving up — the meeting mesh already does this, and the
      // 1:1 path silently aborting is why clicking "call" looked like nothing
      // happened at all.
      if(video){
        try{ stream=await navigator.mediaDevices.getUserMedia({audio:true}); video=false; CALL.video=false;
             toasty("No camera — starting a voice call instead","ok"); }
        catch(err2){ callDeviceError(err2); endCall(false); return; }
      } else { callDeviceError(err); endCall(false); return; }
    }
    CALL.local=stream;openCallUI(peer,video,"Calling…");
    const pc=newPC(await iceConfig());stream.getTracks().forEach(t=>pc.addTrack(t,stream));
    const offer=await pc.createOffer();await pc.setLocalDescription(offer);
    sendSignal(peer,"offer",{sdp:pc.localDescription,video});
  }catch(e){callDeviceError(e);endCall(false);}
}
async function onSignal(msg){
  const from=msg.from,kind=msg.kind,p=msg.payload||{};
  if(kind&&kind.startsWith("meeting-")){onMeetingSignal(kind,p);return;}
  if(kind&&kind.startsWith("m-")){onMeetMesh(kind,from,p);return;}
  if(kind==="offer"){
    if(CALL.pc){sendSignal(from,"hangup",{});return;} // busy
    CALL.incoming={from,sdp:p.sdp,video:p.video};showIncoming(from,p.video);
  }else if(kind==="answer"){
    if(CALL.pc){try{await CALL.pc.setRemoteDescription(p.sdp);}catch(_){}}
  }else if(kind==="ice"){
    if(CALL.pc&&p.candidate){try{await CALL.pc.addIceCandidate(p.candidate);}catch(_){}}
  }else if(kind==="hangup"){
    if(CALL.incoming&&CALL.incoming.from===from){hideIncoming();CALL.incoming=null;toasty("Call ended","ok");}
    else endCall(false);
  }
}
async function acceptCall(){
  const inc=CALL.incoming;if(!inc)return;hideIncoming();CALL.incoming=null;
  try{
    CALL.peer=inc.from;CALL.video=inc.video;
    const stream=await navigator.mediaDevices.getUserMedia({audio:true,video:inc.video});
    CALL.local=stream;openCallUI(inc.from,inc.video,"Connecting…");
    const pc=newPC(await iceConfig());stream.getTracks().forEach(t=>pc.addTrack(t,stream));
    await pc.setRemoteDescription(inc.sdp);
    const ans=await pc.createAnswer();await pc.setLocalDescription(ans);
    sendSignal(inc.from,"answer",{sdp:pc.localDescription});
  }catch(e){toasty("Cannot access mic/camera","err");endCall(true);}
}
function declineCall(){if(CALL.incoming){sendSignal(CALL.incoming.from,"hangup",{});hideIncoming();CALL.incoming=null;}}
function endCall(notify){
  if(notify!==false&&CALL.peer)sendSignal(CALL.peer,"hangup",{});
  if(CALL.pc){try{CALL.pc.close();}catch(_){}}
  if(CALL.local)CALL.local.getTracks().forEach(t=>t.stop());
  if(CALL_SCREEN){CALL_SCREEN.getTracks().forEach(t=>t.stop());CALL_SCREEN=null;}
  CALL_ORIG_VIDEO=null;
  CALL={pc:null,peer:null,local:null,video:false,incoming:null,status:""};
  closeCallUI();
}
function setCallStatus(s){CALL.status=s;const el=document.getElementById("call-status");if(el)el.textContent=s;}
function openCallUI(peer,video,status){
  document.getElementById("call-name").textContent=memberName(peer);
  setCallStatus(status);
  const rv=document.getElementById("call-remote"),lv=document.getElementById("call-local");
  if(CALL.local)lv.srcObject=CALL.local;
  lv.style.display=video?"":"none";rv.classList.toggle("audioonly",!video);
  document.getElementById("call-cam").style.display=video?"":"none";
  document.getElementById("call-mute").classList.remove("off");
  document.getElementById("ov-call").classList.add("open");
}
function closeCallUI(){const o=document.getElementById("ov-call");if(o)o.classList.remove("open");
  ["call-remote","call-local"].forEach(id=>{const v=document.getElementById(id);if(v)v.srcObject=null;});}
function showIncoming(from,video){
  document.getElementById("inc-name").textContent=memberName(from);
  document.getElementById("inc-kind").textContent=(video?"Video":"Voice")+" call";
  document.getElementById("ov-incoming").classList.add("open");
  chime();
}
function hideIncoming(){const o=document.getElementById("ov-incoming");if(o)o.classList.remove("open");}
function toggleMute(){const t=CALL.local&&CALL.local.getAudioTracks()[0];if(t){t.enabled=!t.enabled;document.getElementById("call-mute").classList.toggle("off",!t.enabled);}}
function toggleCam(){const t=CALL.local&&CALL.local.getVideoTracks()[0];if(t){t.enabled=!t.enabled;document.getElementById("call-cam").classList.toggle("off",!t.enabled);}}
let CALL_SCREEN=null, CALL_ORIG_VIDEO=null;
async function callShare(){
  const btn=document.getElementById("call-share");
  if(CALL_SCREEN){
    CALL_SCREEN.getTracks().forEach(t=>t.stop());CALL_SCREEN=null;
    if(CALL.pc&&CALL_ORIG_VIDEO){const s=CALL.pc.getSenders().find(s=>s.track&&s.track.kind==='video');if(s)s.replaceTrack(CALL_ORIG_VIDEO).catch(()=>{});}
    btn.classList.remove("on");toasty("Screen sharing stopped","ok");return;
  }
  try{
    const stream=await navigator.mediaDevices.getDisplayMedia({video:true,audio:false});
    CALL_SCREEN=stream;const track=stream.getVideoTracks()[0];
    if(!track){toasty("No screen track","err");return;}
    CALL_ORIG_VIDEO=CALL.local?.getVideoTracks()[0]||null;
    if(CALL.pc){const s=CALL.pc.getSenders().find(s=>s.track&&s.track.kind==='video');if(s)s.replaceTrack(track).catch(()=>{});}
    track.onended=()=>{CALL_SCREEN=null;if(CALL.pc&&CALL_ORIG_VIDEO){const s=CALL.pc.getSenders().find(s=>s.track&&s.track.kind==='video');if(s)s.replaceTrack(CALL_ORIG_VIDEO).catch(()=>{});}btn.classList.remove("on");toasty("Screen sharing ended","ok");};
    btn.classList.add("on");toasty("Sharing your screen","ok");
  }catch(e){if(e.name!=="AbortError")toasty("Could not share: "+e.message,"err");}
}
function toggleFullscreen(cls){
  const el=document.querySelector('.'+cls);
  if(!el)return;
  if(document.fullscreenElement){
    document.exitFullscreen();
  }else{
    el.requestFullscreen().catch(()=>{});
  }
}
// Call the other participant of the current DM.
function callCurrent(video){
  const ch=currentChannel();const me=(ME&&ME.username)||"user";
  const other=(ch.members||[]).find(u=>u!==me);
  if(!other){toasty("Open a direct message to call","err");return;}
  startCall(other,video);
}
// ── Meetings: calendar, reminders/rings, and the mesh meeting room ───────────
let MEETINGS=[]; let MEET={id:null,pcs:{},streams:{},local:null,meta:null,screen:null};
let MEETRING=null; // pending ring {meeting}
let MEET_CAL={y:null,m:null,sel:null}; // selected date YYYY-MM-DD
async function loadMeetings(){try{MEETINGS=await(await fetch("/api/meetings")).json();}catch(e){MEETINGS=[];}renderMeetUpNext();try{renderMeetCal();}catch(e){}}
function meetWhen(m){const d=new Date(m.start);const today=new Date().toDateString()===d.toDateString();
  const t=d.toLocaleTimeString([],{hour:"2-digit",minute:"2-digit"});
  return today?t:d.toLocaleDateString([],{month:"short",day:"numeric"})+" "+t;}
function meetLive(m){const s=new Date(m.start).getTime();return Date.now()>=s&&Date.now()<s+m.duration_min*60000;}
function meetingCounts(){const m=new Map();MEETINGS.forEach(mt=>{
  const d=new Date(mt.start).toISOString().slice(0,10);
  if(new Date(mt.start).getTime()+mt.duration_min*60000>Date.now())m.set(d,(m.get(d)||0)+1);
});return m;}
// The rail shows what is NEXT, not a month of empty squares. The full month
// lives in the Calendar view, one click away.
function renderMeetUpNext(){
  const el=document.getElementById("meet-upnext");if(!el)return;
  const now=Date.now();
  const up=(MEETINGS||[])
    .filter(m=>!m.cancelled)
    .map(m=>({m,start:Date.parse(m.start)||0}))
    .filter(x=>x.start+60*60*1000>now)      // keep a meeting visible while it runs
    .sort((a,b)=>a.start-b.start)
    .slice(0,3);
  railCount("count-meet",(MEETINGS||[]).filter(m=>!m.cancelled).length);
  if(!up.length){
    el.innerHTML='<div class="meet-empty">No meetings scheduled · <span style="color:var(--accent2);cursor:pointer" onclick="openMeetModal()">book one</span></div>';
    applyRailFold();return;
  }
  el.innerHTML=up.map(({m,start})=>{
    const d=new Date(start);
    const today=d.toDateString()===new Date().toDateString();
    const when=today?d.toLocaleTimeString([],{hour:"2-digit",minute:"2-digit"})
                    :d.toLocaleDateString([],{weekday:"short"})+" "+d.toLocaleTimeString([],{hour:"2-digit",minute:"2-digit"});
    const live=meetLive(m);
    const n=(m.participants||[]).length;
    return `<div class="meet-up" onclick="openMeeting('${esc(m.id)}')" title="${esc(m.title)} · ${n} người">
      ${live?'<span class="mu-live"></span>':`<span class="mu-time">${esc(when)}</span>`}
      <span class="mu-title">${esc(m.title)}</span>
      ${live?`<span class="mu-join" onclick="event.stopPropagation();joinMeeting('${esc(m.id)}')">Join</span>`:`<span class="mu-time">${n}👤</span>`}</div>`;
  }).join("");
  applyRailFold();
}

function renderMeetCal(){const el=document.getElementById("meet-cal");if(!el)return;
  const dl=document.getElementById("meet-daylist");
  const now=new Date();const counts=meetingCounts();
  if(!MEET_CAL.y){MEET_CAL.y=now.getFullYear();MEET_CAL.m=now.getMonth();MEET_CAL.sel=now.toISOString().slice(0,10);}
  const y=MEET_CAL.y,m=MEET_CAL.m;
  const first=new Date(y,m,1);const startDow=(first.getDay()+6)%7;
  const daysInMonth=new Date(y,m+1,0).getDate();
  const daysInPrev=new Date(y,m,0).getDate();
  const mn=first.toLocaleString([],{month:"long"});
  const DOW=["M","T","W","T","F","S","S"];
  const dows=DOW.map(d=>`<div class="mc-dow">${d}</div>`).join("");
  const todayStr=now.toISOString().slice(0,10);
  let cells="";
  for(let i=startDow-1;i>=0;i--){const d=daysInPrev-i;cells+=`<div class="mc-day other"><span class="mc-num">${d}</span></div>`;}
  for(let d=1;d<=daysInMonth;d++){
    const ds=`${y}-${String(m+1).padStart(2,'0')}-${String(d).padStart(2,'0')}`;
    const isToday=ds===todayStr;const isSel=ds===MEET_CAL.sel;
    const n=counts.get(ds)||0;
    let cls='mc-day';if(isToday)cls+=' today';if(isSel)cls+=' sel';
    let dots='';if(n>0){dots='<div class="mc-dot">';for(let j=0;j<Math.min(n,3);j++)dots+='<span class="d"></span>';dots+='</div>';}
    cells+=`<div class="${cls}" onclick="selectMeetDay('${ds}')"><span class="mc-num">${d}</span>${dots}</div>`;
  }
  const rem=7-((startDow+daysInMonth)%7);if(rem<7){for(let d=1;d<=rem;d++)cells+=`<div class="mc-day other"><span class="mc-num">${d}</span></div>`;}
  setHTML(el,`<div class="mc-hd"><button onclick="meetCalNav(-1)" title="Previous month"><i class="ti ti-chevron-left"></i></button><span>${mn} ${y}</span><button onclick="meetCalNav(1)" title="Next month"><i class="ti ti-chevron-right"></i></button></div>
    <div class="mc-grid">${dows}${cells}</div>`);
  renderMeetDayList(dl,counts);}
function meetCalNav(dir){MEET_CAL.m+=dir;if(MEET_CAL.m<0){MEET_CAL.m=11;MEET_CAL.y--;}else if(MEET_CAL.m>11){MEET_CAL.m=0;MEET_CAL.y++;}
  renderMeetCal();}
function selectMeetDay(ds){MEET_CAL.sel=ds;renderMeetCal();}
function renderMeetDayList(el,counts){
  if(!MEET_CAL.sel){el.classList.remove("on");return;}
  const ms=MEETINGS.filter(m=>{
    const d=new Date(m.start).toISOString().slice(0,10);
    return d===MEET_CAL.sel&&new Date(m.start).getTime()+m.duration_min*60000>Date.now();
  }).sort((a,b)=>new Date(a.start)-new Date(b.start));
  if(!ms.length){el.classList.remove("on");return;}
  const d=new Date(MEET_CAL.sel+"T12:00");
  const hd=d.toLocaleDateString([],{weekday:"short",month:"short",day:"numeric"});
  setHTML(el,`<div class="md-hd">${hd}</div>${ms.map(m=>`<div class="meet-item ${meetLive(m)?'live':''}" onclick="openMeeting('${m.id}')" title="${esc(m.title)} — ${(m.participants||[]).map(memberName).map(esc).join(', ')}">
      <i class="ti ti-calendar-event"></i><span class="mi-t">${esc(m.title)}</span>
      <span class="mi-when">${meetLive(m)?'LIVE':esc(meetWhen(m))}</span></div>`).join("")}`);
  el.classList.add("on");}
let MT_SEL=[]; // selected participants [{username, name}]
let MT_DETAIL=null; // current meeting being viewed in detail popup
function openMeetModal(m){
  MT_SEL=[];MT_DETAIL=null;
  document.getElementById("mt-edit-id").value=m?m.id:"";
  document.getElementById("mt-save-lbl").textContent=m?"Save":"Book";
  document.getElementById("mt-title").value=m?m.title||"":"";
  document.getElementById("mt-agenda").value=m?(m.agenda||""):"";
  const s=m?new Date(m.start):new Date(Date.now()+15*60000);
  const e=m?new Date(s.getTime()+(m.duration_min||30)*60000):new Date(s.getTime()+30*60000);
  document.getElementById("mt-date").value=s.toISOString().slice(0,10);
  document.getElementById("mt-time").value=s.toTimeString().slice(0,5);
  document.getElementById("mt-endtime").value=e.toTimeString().slice(0,5);
  const rm=m&&m.remind_min!==undefined?String(m.remind_min):"10";
  const sel=document.getElementById("mt-remind");
  for(let o of sel.options)o.selected=o.value===rm;
  document.getElementById("mt-msg").textContent="";
  if(m&&m.participants){
    MT_SEL=(m.participants||[]).filter(u=>u!==(ME&&ME.username)).map(u=>({username:u,name:memberName(u)}));
  }
  renderMtChips();
  document.getElementById("mt-search").value="";
  document.getElementById("mt-drop").style.display="none";
  document.getElementById("ov-meet").classList.add("open");
  setTimeout(()=>document.getElementById("mt-title").focus(),40);}
function mtSearch(q){
  const drop=document.getElementById("mt-drop");
  if(!q||q.length<1){drop.style.display="none";return;}
  const me=(ME&&ME.username)||"";
  const sel=new Set(MT_SEL.map(s=>s.username));
  const hits=MEMBERS.filter(u=>u.username!==me&&!sel.has(u.username)&&(memberName(u.username).toLowerCase().includes(q.toLowerCase())||u.username.toLowerCase().includes(q.toLowerCase()))).slice(0,8);
  if(!hits.length){drop.style.display="none";return;}
  drop.innerHTML=hits.map(u=>`<div class="mtd-item" onclick="mtAdd('${esc(u.username)}','${esc(memberName(u.username))}')">${esc(memberName(u.username))} <span style="color:var(--dim);font-size:11px">@${esc(u.username)}</span></div>`).join("");
  drop.style.display="block";}
function mtKey(e){const drop=document.getElementById("mt-drop");const items=drop.querySelectorAll(".mtd-item");
  if(e.key==="ArrowDown"){e.preventDefault();if(items.length){items[0].classList.add("sel");items[0].focus();}}
  else if(e.key==="Enter"){e.preventDefault();const sel=drop.querySelector(".mtd-item.sel");if(sel)sel.click();}
  else if(e.key==="Escape"){drop.style.display="none";}}
function mtAdd(username,name){
  if(!MT_SEL.find(s=>s.username===username))MT_SEL.push({username,name});
  renderMtChips();document.getElementById("mt-search").value="";document.getElementById("mt-drop").style.display="none";}
function mtRemove(username){MT_SEL=MT_SEL.filter(s=>s.username!==username);renderMtChips();}
function renderMtChips(){
  document.getElementById("mt-chips").innerHTML=MT_SEL.map(s=>`<span class="mt-chip">${esc(s.name)}<button onclick="mtRemove('${esc(s.username)}')">&times;</button></span>`).join("");}
function calSyncDuration(){
  const t=document.getElementById("mt-time").value;const e=document.getElementById("mt-endtime").value;
  if(t&&e){const d=(new Date("2000-01-01T"+e)-new Date("2000-01-01T"+t))/60000;
    if(d>0)document.getElementById("mt-remind").dataset.dur=d;}}
async function saveMeeting(){const msg=document.getElementById("mt-msg");
  const title=document.getElementById("mt-title").value.trim();
  if(!title){msg.textContent="Give the meeting a title.";return;}
  const start=new Date(document.getElementById("mt-date").value+"T"+document.getElementById("mt-time").value);
  if(isNaN(start)||start.getTime()<Date.now()-60000){msg.textContent="Pick a future time.";return;}
  let dur=30;const et=document.getElementById("mt-endtime").value;
  if(et){dur=Math.round((new Date("2000-01-01T"+et)-new Date("2000-01-01T"+document.getElementById("mt-time").value))/60000);if(dur<5)dur=5;}
  const participants=MT_SEL.map(s=>s.username);
  const agenda=document.getElementById("mt-agenda").value.trim();
  const editId=document.getElementById("mt-edit-id").value;
  const body={title,start:start.toISOString(),duration_min:dur,participants,remind_min:parseInt(document.getElementById("mt-remind").value,10),agenda:agenda||undefined};
  try{const method=editId?"PATCH":"POST";const url=editId?`/api/meetings/${editId}`:"/api/meetings";
    const r=await fetch(url,{method,headers:{"Content-Type":"application/json"},body:JSON.stringify(body)});
    if(r.ok){close_("ov-meet");toasty(editId?"Meeting updated":"Meeting booked — invitations sent","ok");loadMeetings();renderCalendar();}
    else msg.textContent=await r.text()||"Could not save.";
  }catch(e){msg.textContent="Network error.";}}
function openMeeting(id){const m=MEETINGS.find(x=>x.id===id);if(!m)return;
  MT_DETAIL=m;
  document.getElementById("md-title").textContent=m.title||"Meeting";
  document.getElementById("md-when").textContent=meetWhenDetail(m);
  document.getElementById("md-participants").innerHTML=(m.participants||[]).map(u=>`<span class="mt-chip">${esc(memberName(u))}</span>`).join("");
  const agendaEl=document.getElementById("md-agenda");
  if(m.agenda){agendaEl.style.display="block";agendaEl.textContent=m.agenda;}else agendaEl.style.display="none";
  const mine=ME&&(m.created_by===ME.username||(ME&&ME.role==="super")||(ME&&ME.role==="admin"));
  document.getElementById("md-edit").style.display=mine?"":"none";
  document.getElementById("md-cancel").style.display=mine?"":"none";
  const canJoin=Date.now()>=new Date(m.start).getTime()-5*60000;
  document.getElementById("md-join").style.display=canJoin?"":"none";
  document.getElementById("md-ring").style.display=canJoin?"":"none";
  // Always allow joining if meeting hasn't ended
  const stillValid=new Date(m.start).getTime()+m.duration_min*60000>Date.now();
  document.getElementById("md-start").style.display=(stillValid&&!canJoin)?"":"none";
  document.getElementById("ov-mtdetail").classList.add("open");}
function meetWhenDetail(m){const s=new Date(m.start);const e=new Date(s.getTime()+m.duration_min*60000);
  const fmt=(d)=>d.toLocaleDateString([],{weekday:"long",month:"long",day:"numeric"})+" · "+d.toLocaleTimeString([],{hour:"2-digit",minute:"2-digit"});
  return fmt(s)+" – "+e.toLocaleTimeString([],{hour:"2-digit",minute:"2-digit"});}
function mtJoinFromDetail(){if(MT_DETAIL){close_("ov-mtdetail");joinMeeting(MT_DETAIL.id);}}
async function mtCancelDetail(){if(!MT_DETAIL||!confirm("Cancel this meeting?"))return;
  try{const r=await fetch("/api/meetings/"+MT_DETAIL.id,{method:"PATCH",headers:{"Content-Type":"application/json"},body:JSON.stringify({cancel:true})});
    if(r.ok){close_("ov-mtdetail");toasty("Meeting cancelled","ok");loadMeetings();renderCalendar();}
    else toasty(await r.text()||"Cannot cancel","err");}catch(e){toasty("Network error","err");}}
function mtEditDetail(){const m=MT_DETAIL;close_("ov-mtdetail");if(m)openMeetModal(m);}
function mtRingAll(){const m=MT_DETAIL;if(!m)return;(m.participants||[]).forEach(u=>{if(u!==(ME&&ME.username))meetSignal(u,"m-hello",{});});toasty(`Ringing all participants…`,"ok");}
async function cancelMeeting(id){try{const r=await fetch("/api/meetings/"+id,{method:"PATCH",headers:{"Content-Type":"application/json"},body:JSON.stringify({cancel:true})});
    if(r.ok){toasty("Meeting cancelled","ok");loadMeetings();renderCalendar();}else toasty(await r.text()||"Cannot cancel","err");}catch(e){toasty("Network error","err");}}
// Reminder / start / ring frames from the server (or a nudging teammate).
function onMeetingSignal(kind,p){const m=(p&&p.meeting)||{};
  if(kind==="meeting-invite"){toasty(`📅 ${memberName(m.created_by)} invited you: ${m.title} · ${meetWhen(m)}`,"ok");loadMeetings();return;}
  if(kind==="meeting-cancel"){toasty(`📅 Meeting cancelled: ${m.title}`,"ok");loadMeetings();return;}
  if(kind==="meeting-remind"){toasty(`⏰ ${m.title} starts at ${meetWhen(m)}`,"ok");chime();sysNotify(`Meeting soon: ${m.title}`,`Starts ${meetWhen(m)}`);return;}
  if(kind==="meeting-start"){toasty(`📅 ${m.title} is starting — join from the Meetings list`,"ok");chime();sysNotify(`Meeting started: ${m.title}`,"Click to join");loadMeetings();return;}
  if(kind==="meeting-ring"){
    if(MEET.id===m.id)return; // already in the room
    MEETRING={meeting:m};
    document.getElementById("mr-title").textContent=m.title||"Meeting";
    document.getElementById("mr-sub").textContent="You're being called into this meeting";
    document.getElementById("ov-meetring").classList.add("open");chime();
    sysNotify(`📞 ${m.title}`,"You're being called into the meeting");}}
function sysNotify(title,body){try{
  if(window.Notification&&Notification.permission==="granted"&&!document.hasFocus())new Notification(title,{body});
}catch(e){}}
function dismissMeetRing(){MEETRING=null;document.getElementById("ov-meetring").classList.remove("open");}
function joinFromRing(){const r=MEETRING;dismissMeetRing();if(r&&r.meeting)joinMeeting(r.meeting.id);}
// ── The room: full-mesh WebRTC (one RTCPeerConnection per present peer) ──────
function meetSignal(to,kind,payload){payload=payload||{};payload.mid=MEET.id;sendSignal(to,kind,payload);}
async function joinMeeting(id){
  if(CALL.pc){toasty("Finish your current call first","err");return;}
  if(MEET.id===id)return;
  if(MEET.id)leaveRoom(false);
  let m;try{const r=await fetch("/api/meetings/"+id+"/join",{method:"POST"});
    if(!r.ok){toasty(await r.text()||"Cannot join","err");return;}m=await r.json();
  }catch(e){toasty("Network error","err");return;}
  try{MEET.local=await navigator.mediaDevices.getUserMedia({audio:true,video:true});}
  catch(e){try{MEET.local=await navigator.mediaDevices.getUserMedia({audio:true});}
    catch(e2){MEET.local=new MediaStream();toasty("No mic/camera — joining view-only","ok");}}
  MEET.id=id;MEET.meta=m;MEET.pcs={};MEET.streams={};
  openRoomUI(m);
  // Announce to everyone invited; whoever is present offers back to us.
  const me=(ME&&ME.username)||"";
  (m.participants||[]).filter(u=>u!==me).forEach(u=>meetSignal(u,"m-hello",{}));
  loadMeetings();}
async function meetPC(peer){
  if(MEET.pcs[peer])return MEET.pcs[peer];
  const pc=new RTCPeerConnection({iceServers:await iceConfig()});
  MEET.pcs[peer]=pc;
  MEET.local.getTracks().forEach(t=>pc.addTrack(t,MEET.local));
  pc.onicecandidate=e=>{if(e.candidate)meetSignal(peer,"m-ice",{candidate:e.candidate});};
  pc.ontrack=e=>{if(e.streams[0]){MEET.streams[peer]=e.streams[0];renderRoom();}};
  pc.onconnectionstatechange=()=>{if(["failed","closed"].includes(pc.connectionState))dropPeer(peer);};
  return pc;}
function dropPeer(peer){const pc=MEET.pcs[peer];if(pc){try{pc.close();}catch(_){}}
  delete MEET.pcs[peer];delete MEET.streams[peer];renderRoom();}
async function onMeetMesh(kind,from,p){
  if(!MEET.id||!p||p.mid!==MEET.id){
    // A hello for a room we're not in — ignore; rings handle invitations.
    return;}
  if(kind==="m-hello"){ // newcomer announced — the present side sends the offer
    const pc=await meetPC(from);
    const offer=await pc.createOffer();await pc.setLocalDescription(offer);
    meetSignal(from,"m-offer",{sdp:pc.localDescription});renderRoom();
  }else if(kind==="m-offer"){
    const pc=await meetPC(from);
    await pc.setRemoteDescription(p.sdp);
    const ans=await pc.createAnswer();await pc.setLocalDescription(ans);
    meetSignal(from,"m-answer",{sdp:pc.localDescription});renderRoom();
  }else if(kind==="m-answer"){
    const pc=MEET.pcs[from];if(pc){try{await pc.setRemoteDescription(p.sdp);}catch(_){}}
  }else if(kind==="m-ice"){
    const pc=MEET.pcs[from];if(pc&&p.candidate){try{await pc.addIceCandidate(p.candidate);}catch(_){}}
  }else if(kind==="m-leave"){dropPeer(from);}}
function openRoomUI(m){document.getElementById("room-title").textContent=m.title;
  document.getElementById("ov-room").classList.add("open");renderRoom();}
function renderRoom(){const grid=document.getElementById("room-grid");if(!grid||!MEET.id)return;
  const me=(ME&&ME.username)||"";
  // Build tiles: self + peers + screen share (if active)
  const tileList=[{u:me,stream:MEET.local,self:true},
    ...Object.keys(MEET.pcs).map(u=>({u,stream:MEET.streams[u]}))];
  const screenTile=MEET.screen?[{u:"screen",stream:MEET.screen,self:true}]:[];
  const tiles=[...screenTile,...tileList];
  grid.innerHTML=tiles.map(t=>{
    const isScreen=t.u==="screen";
    const label=isScreen?"Your screen":(esc(memberName(t.u))+(t.self?" (you)":""));
    if(isScreen){
      return `<div class="room-tile screen"><video autoplay playsinline muted data-u="screen"></video><span class="rt-name">📺 ${label}</span></div>`;
    }
    return `<div class="room-tile" id="tile-${esc(t.u)}">
      ${t.stream&&t.stream.getVideoTracks().some(v=>v.enabled)?`<video autoplay playsinline ${t.self?"muted":""} data-u="${esc(t.u)}"></video>`:`<div class="rt-av">${esc((memberName(t.u)||"?").slice(0,2).toUpperCase())}</div>`}
      <span class="rt-name">${label}</span></div>`;
  }).join("");
  tiles.forEach(t=>{const v=grid.querySelector(`video[data-u="${CSS.escape(t.u)}"]`);if(v&&t.stream)v.srcObject=t.stream;});
  document.getElementById("room-count").textContent=`${tileList.length} in room`;
  const present=new Set(tileList.map(t=>t.u));
  const absent=((MEET.meta&&MEET.meta.participants)||[]).filter(u=>!present.has(u));
  document.getElementById("room-absent").innerHTML=absent.map(u=>`<button class="rt-ringbtn" onclick="ringUser('${esc(u)}')" title="Ring ${esc(memberName(u))}"><i class="ti ti-bell-ringing"></i> ${esc(memberName(u))}</button>`).join("");}
async function ringUser(u){try{
  const r=await fetch("/api/meetings/"+MEET.id+"/ring",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({user:u})});
  if(r.ok)toasty(`Ringing ${memberName(u)}…`,"ok");else toasty(await r.text()||"Cannot ring","err");
}catch(e){toasty("Network error","err");}}
function roomMute(){const t=MEET.local&&MEET.local.getAudioTracks()[0];if(t){t.enabled=!t.enabled;document.getElementById("room-mute").classList.toggle("off",!t.enabled);}}
function roomCam(){const t=MEET.local&&MEET.local.getVideoTracks()[0];if(t){t.enabled=!t.enabled;document.getElementById("room-cam").classList.toggle("off",!t.enabled);renderRoom();}}
let MEET_ORIG_VIDEO=null; // original video track before screen share
async function roomShare(){
  const btn=document.getElementById("room-share");
  if(MEET.screen){
    // Stop sharing — revert to camera
    MEET.screen.getTracks().forEach(t=>t.stop());
    MEET.screen=null;
    // Replace screen track with original video in all peer connections
    Object.values(MEET.pcs).forEach(pc=>{
      const sender=pc.getSenders().find(s=>s.track&&s.track.kind==='video');
      if(sender&&MEET_ORIG_VIDEO) sender.replaceTrack(MEET_ORIG_VIDEO).catch(()=>{});
    });
    btn.classList.remove("on");
    toasty("Screen sharing stopped","ok");
    renderRoom();
    return;
  }
  try{
    const stream=await navigator.mediaDevices.getDisplayMedia({video:true,audio:false});
    MEET.screen=stream;
    const screenTrack=stream.getVideoTracks()[0];
    if(!screenTrack){toasty("No screen track","err");return;}
    // Save original video track
    MEET_ORIG_VIDEO=MEET.local.getVideoTracks()[0]||null;
    // Replace video track in all existing peer connections
    Object.values(MEET.pcs).forEach(pc=>{
      const sender=pc.getSenders().find(s=>s.track&&s.track.kind==='video');
      if(sender) sender.replaceTrack(screenTrack).catch(()=>{});
      else pc.addTrack(screenTrack,stream);
    });
    // Listen for stream stop (user clicks browser's stop sharing button)
    screenTrack.onended=()=>{
      MEET.screen=null;
      Object.values(MEET.pcs).forEach(pc=>{
        const sender=pc.getSenders().find(s=>s.track&&s.track.kind==='video');
        if(sender&&MEET_ORIG_VIDEO) sender.replaceTrack(MEET_ORIG_VIDEO).catch(()=>{});
      });
      btn.classList.remove("on");renderRoom();toasty("Screen sharing ended","ok");
    };
    btn.classList.add("on");
    toasty("Sharing your screen","ok");
    renderRoom();
  }catch(e){
    if(e.name!=="AbortError") toasty("Could not share screen: "+e.message,"err");
  }
}
function leaveRoom(notify){
  if(notify!==false)Object.keys(MEET.pcs).forEach(u=>meetSignal(u,"m-leave",{}));
  Object.values(MEET.pcs).forEach(pc=>{try{pc.close();}catch(_){}});
  if(MEET.local)MEET.local.getTracks().forEach(t=>t.stop());
  if(MEET.screen)MEET.screen.getTracks().forEach(t=>t.stop());
  MEET_ORIG_VIDEO=null;
  MEET={id:null,pcs:{},streams:{},local:null,meta:null,screen:null};
  document.getElementById("ov-room").classList.remove("open");
  loadMeetings();}
// Refresh the sidebar clock-face (LIVE badges) once a minute.
setInterval(renderMeetCal,60000);
// ── Calendar page: week + day view ──────────────────────────────────────────
let CAL_VIEW="week"; // "week" | "day"
let CAL_WEEK_START=null; // Monday of current week as Date
function calWeekStart(d){const dt=new Date(d);const dow=(dt.getDay()+6)%7;dt.setDate(dt.getDate()-dow);dt.setHours(0,0,0,0);return dt;}
function fmtDate(d){return d.getFullYear()+'-'+String(d.getMonth()+1).padStart(2,'0')+'-'+String(d.getDate()).padStart(2,'0');}
function renderCalendar(){
  if(!CAL_WEEK_START)CAL_WEEK_START=calWeekStart(new Date());
  const el=document.getElementById("cal-body");if(!el)return;
  const ws=CAL_WEEK_START;const we=new Date(ws);we.setDate(we.getDate()+6);
  const now=new Date();const todayStr=fmtDate(now);
  const days=[];for(let i=0;i<7;i++){const d=new Date(ws);d.setDate(d.getDate()+i);days.push(d);}
  const ms=MEETINGS.filter(m=>{const s=new Date(m.start);return s>=ws&&s<=new Date(we.getTime()+86400000)&&new Date(m.start).getTime()+m.duration_min*60000>Date.now();});
  const dowShort=["Mon","Tue","Wed","Thu","Fri","Sat","Sun"];
  const dowFull=["Monday","Tuesday","Wednesday","Thursday","Friday","Saturday","Sunday"];
  const mnFull=["January","February","March","April","May","June","July","August","September","October","November","December"];
  const isDay=CAL_VIEW==="day";

  // Header day cells
  let hdrCells="";
  if(isDay){
    const selDay=days.find(d=>fmtDate(d)===todayStr)||days[0];
    hdrCells=`<div class="gd today" onclick="calPickDay('${fmtDate(selDay)}')"><span class="dn">${selDay.getDate()}</span> ${dowFull[(selDay.getDay()+6)%7]}</div>`;
  }else{
    hdrCells=days.map(d=>{
      const ds=fmtDate(d);
      return `<div class="gd${ds===todayStr?' today':''}" onclick="calPickDay('${ds}')"><span class="dn">${d.getDate()}</span> ${dowShort[(d.getDay()+6)%7]}</div>`;
    }).join("");
  }

  // Gutter hours
  const gutter=Array.from({length:24},(_,i)=>`<div class="ghour">${String(i).padStart(2,'0')}:00</div>`).join("");

  // Columns
  let cols="";
  if(isDay){
    const selDay=days.find(d=>fmtDate(d)===todayStr)||days[0];
    const ds=fmtDate(selDay);
    cols=`<div class="cal-col">${Array.from({length:24},(_,h)=>`<div class="ch" onclick="calClickSlot('${ds}',${h})" style="--i:${h}"></div><div class="chl" style="--i:${h}"></div><div class="chl half" style="--i:${h}"></div>`).join("")}</div>`;
  }else{
    cols=days.map(d=>`<div class="cal-col">${Array.from({length:24},(_,h)=>`<div class="ch" onclick="calClickSlot('${fmtDate(d)}',${h})" style="--i:${h}"></div><div class="chl" style="--i:${h}"></div><div class="chl half" style="--i:${h}"></div>`).join("")}</div>`).join("");
  }

  // Meeting blocks
  // Meeting blocks with overlap detection (side-by-side like Google Calendar)
  const blocks=(()=>{
    // Group meetings by day, sort by start time
    const byDay={};ms.forEach(m=>{const ds=fmtDate(new Date(m.start));(byDay[ds]=byDay[ds]||[]).push(m);});
    let html="";
    Object.entries(byDay).forEach(([ds,dayMs])=>{
      dayMs.sort((a,b)=>new Date(a.start)-new Date(b.start));
      const di=days.findIndex(d=>fmtDate(d)===ds);
      if(!isDay&&di<0)return;
      // Assign each meeting to a column (0-based) and total columns in its max-overlap group
      const cols=[]; // [{col:0, total:1}] per meeting
      const active=[]; // currently active meetings (end times)
      for(const m of dayMs){
        const s=new Date(m.start).getTime();
        const e=s+m.duration_min*60000;
        // Remove ended meetings
        for(let i=active.length-1;i>=0;i--){if(active[i]<=s)active.splice(i,1);}
        // Find first free column
        let col=0;while(col<active.length&&active[col]>s)col++;
        active[col]=e;
        cols.push({col,total:active.length});
      }
      // Recompute total: for each meeting, max total across its time span
      const totals=cols.map((_,i)=>{
        const s=new Date(dayMs[i].start).getTime();
        const e=s+dayMs[i].duration_min*60000;
        const endTimes=dayMs.map((m,j)=>new Date(m.start).getTime()+m.duration_min*60000);
        let cnt=0;for(let j=0;j<dayMs.length;j++){const js=new Date(dayMs[j].start).getTime();if(js<e&&endTimes[j]>s)cnt++;}
        return cnt;
      });
      dayMs.forEach((m,i)=>{
        const col=cols[i].col;
        const total=totals[i];
        const left=isDay
          ?`calc(${col}*100%/${total} + 2px)`
          :`calc(${di}*100%/7 + ${col}*100%/7/${total} + 2px)`;
        const w=isDay
          ?`calc(100%/${total} - 4px)`
          :`calc(100%/7/${total} - 4px)`;
        html+=calBlock2(m,left,w);
      });
    });
    return html;
  })();

  // Current time line
  const nowT=now.getHours()+now.getMinutes()/60;
  const nowLine=CAL_VIEW==="day"||(fmtDate(now)>=fmtDate(ws)&&fmtDate(now)<=fmtDate(we))
    ?`<div class="cal-now" style="--t:${nowT}" title="Now"></div>`:"";

  // Top nav
  const hdrDate=isDay
    ?`${dowFull[((days.find(d=>fmtDate(d)===todayStr)||days[0]).getDay()+6)%7]} ${days.find(d=>fmtDate(d)===todayStr)?.getDate()||ws.getDate()} ${mnFull[ws.getMonth()]} ${ws.getFullYear()}`
    :`${ws.getDate()} ${mnFull[ws.getMonth()]} – ${we.getDate()} ${mnFull[we.getMonth()]} ${we.getFullYear()}`;

  const top=`<div class="cal-top"><h2>${hdrDate}</h2>
    <div class="cal-nav">
      <button onclick="calNav(${isDay?-1:-7})" title="Previous">◀</button>
      <button onclick="CAL_WEEK_START=calWeekStart(new Date());CAL_VIEW='${CAL_VIEW}';renderCalendar()">Today</button>
      <button onclick="calNav(${isDay?1:7})" title="Next">▶</button>
      <button class="${!isDay?'on':''}" onclick="CAL_VIEW='week';renderCalendar()">Week</button>
      <button class="${isDay?'on':''}" onclick="CAL_VIEW='day';renderCalendar()">Day</button>
    </div></div>`;

  el.innerHTML=top+`<div class="cal-wrap${isDay?' day':''}">
    <div class="cal-hdr"><div class="cal-ghdr"></div><div class="cal-dhdr">${hdrCells}</div></div>
    <div class="cal-scroll" id="cal-scroll">
      <div class="cal-ghdr">${gutter}</div>
      <div class="cal-body">${nowLine}${cols}${blocks}</div>
    </div></div>`;
  // Auto-scroll to current hour (7am if earlier)
  setTimeout(()=>{
    const s=document.getElementById("cal-scroll");
    if(s){const h=Math.max(new Date().getHours()-2,0);s.scrollTop=h*60;}
  },50);
}
function calBlock2(m,left,w){
  const s=new Date(m.start);const e=new Date(s.getTime()+m.duration_min*60000);
  const t=s.getHours()+s.getMinutes()/60;
  const h=Math.max((e-s)/3600000,0.5);
  const live=meetLive(m);
  return `<div class="cb${live?' live':''}" style="--t:${t};--h:${h};left:${left};width:${w}" onclick="openMeeting('${m.id}')" title="${esc(m.title)} — ${meetWhen(m)}">
      <div class="cbt">${esc(m.title)}</div>
      <div class="cbs">${meetWhen(m)}${(m.participants||[]).length?` · ${(m.participants||[]).slice(0,3).map(memberName).join(", ")}`:""}</div></div>`;
}
function calClickSlot(ds,hour){
  const slot=new Date(ds+"T"+String(Math.max(hour,0)).padStart(2,'0')+":00");
  if(slot<new Date(Date.now()+15*60000))return;
  document.getElementById("mt-date").value=ds;
  document.getElementById("mt-time").value=slot.toTimeString().slice(0,5);
  document.getElementById("mt-title").value="";
  document.getElementById("mt-msg").textContent="";
  openMeetModalQuick();
}
function openMeetModalQuick(){openMeetModal();}
function calNav(days){CAL_WEEK_START.setDate(CAL_WEEK_START.getDate()+days);renderCalendar();}
function calPickDay(ds){CAL_VIEW="day";CAL_WEEK_START=calWeekStart(new Date(ds));renderCalendar();}
// ── Emoji picker (shared: composer insert + message reactions) ───────────────
const EMOJI_CATS=[
  {n:"Smileys",i:"mood-smile",e:"😀 😃 😄 😁 😆 😅 🤣 😂 🙂 🙃 😉 😊 😇 🥰 😍 🤩 😘 😗 😚 😙 😋 😛 😜 🤪 😝 🤑 🤗 🤭 🤫 🤔 🤐 😐 😑 😶 😏 😒 🙄 😬 😮‍💨 🤥 😌 😔 😪 🤤 😴 😷 🤒 🤕 🤢 🤮 🤧 🥵 🥶 🥴 😵 🤯 🤠 🥳 😎 🤓 🧐 😕 😟 🙁 😮 😯 😲 😳 🥺 😦 😧 😨 😰 😥 😢 😭 😱 😖 😣 😞 😓 😩 😫 🥱 😤 😡 😠 🤬 😈 👿 💀 💩 🤡 👻 👽 🤖".split(" ")},
  {n:"Gestures",i:"hand-stop",e:"👍 👎 👌 🤌 🤏 ✌️ 🤞 🤟 🤘 🤙 👈 👉 👆 👇 ☝️ 👋 🤚 🖐️ ✋ 🖖 👏 🙌 🤝 🙏 💪 🦾 ✍️ 👀 🧠 👂 👃 👅 👄 💋 ❤️‍🔥 💯".split(" ")},
  {n:"Hearts",i:"heart",e:"❤️ 🧡 💛 💚 💙 💜 🤎 🖤 🤍 💔 ❤️‍🩹 💕 💞 💓 💗 💖 💘 💝 💟 ✨ ⭐ 🌟 💫 🔥 🎉 🎊 🥂 🍾".split(" ")},
  {n:"Animals",i:"paw",e:"🐶 🐱 🐭 🐹 🐰 🦊 🐻 🐼 🐨 🐯 🦁 🐮 🐷 🐸 🐵 🐔 🐧 🐦 🐤 🦆 🦅 🦉 🐺 🐗 🐴 🦄 🐝 🐛 🦋 🐌 🐞 🐢 🐍 🐙 🦑 🦐 🐠 🐟 🐬 🐳 🐋 🦈".split(" ")},
  {n:"Food",i:"coffee",e:"🍏 🍎 🍐 🍊 🍋 🍌 🍉 🍇 🍓 🫐 🍒 🍑 🥭 🍍 🥥 🥝 🍅 🍆 🥑 🥦 🌽 🥕 🍞 🧀 🥚 🍳 🥞 🥓 🍔 🍟 🍕 🌭 🥪 🌮 🌯 🍜 🍝 🍣 🍱 🍚 🍦 🍰 🎂 🍩 🍪 🍫 🍿 ☕ 🍵 🍺 🍻 🥂 🍷 🥤".split(" ")},
  {n:"Activity",i:"ball-football",e:"⚽ 🏀 🏈 ⚾ 🎾 🏐 🏉 🎱 🏓 🏸 🥅 🏒 🏑 🏏 ⛳ 🏹 🎣 🥊 🥋 🎽 ⛸️ 🎿 🛷 🏂 🏆 🏅 🥇 🥈 🥉 🎮 🎲 🎯 🎳 🎸 🎹 🥁 🎺 🎬 🎨".split(" ")},
  {n:"Travel",i:"plane",e:"🚗 🚕 🚙 🚌 🏎️ 🚓 🚑 🚒 🚚 🚜 🏍️ 🚲 ✈️ 🚀 🛸 🚁 ⛵ 🚤 🚢 🗽 🗼 🏰 🏠 🏢 🏥 🏦 🏬 🌋 🏔️ 🏝️ 🏖️ 🌅 🌇 🌆 🌉 🌃 🎇".split(" ")},
  {n:"Objects",i:"bulb",e:"💻 🖥️ 🖨️ ⌨️ 🖱️ 💾 💿 📱 ☎️ 📞 📟 📠 📺 📷 🎥 🔋 🔌 💡 🔦 📚 📖 📝 ✏️ 📌 📎 📏 🔒 🔑 🔨 🛠️ ⚙️ 🧲 🔬 🔭 📡 💊 💉 🩺 🧬 🚀 ⏰ 📅 📈 📉 📊 💰 💳 💎".split(" ")},
  {n:"Symbols",i:"check",e:"✅ ❌ ❓ ❗ ‼️ ⁉️ ⚠️ 🚫 ✔️ ➕ ➖ ✖️ ➗ ♾️ 💲 🔴 🟠 🟡 🟢 🔵 🟣 ⚫ ⚪ 🟥 🟧 🟨 🟩 🟦 🟪 🔺 🔻 ▶️ ⏸️ ⏹️ 🔔 🔕 ✨ 🎵 🎶 ©️ ®️ ™️".split(" ")},
];
let EMOJI_TARGET={cat:0,cb:null};
function openEmoji(anchorEl,cb){
  EMOJI_TARGET={cat:0,cb};
  let pop=document.getElementById("emoji-pop");
  if(!pop){pop=document.createElement("div");pop.id="emoji-pop";pop.className="emojipop";document.body.appendChild(pop);
    pop.addEventListener("click",e=>e.stopPropagation());}
  renderEmoji("");
  pop.style.display="block";
  // position above the anchor, clamped to viewport
  const r=anchorEl.getBoundingClientRect();
  const w=320,h=Math.min(360,window.innerHeight-40);
  let left=Math.min(Math.max(8,r.left),window.innerWidth-w-8);
  let top=r.top-h-8; if(top<8)top=Math.min(r.bottom+8,window.innerHeight-h-8);
  pop.style.left=left+"px";pop.style.top=top+"px";pop.style.width=w+"px";
  setTimeout(()=>document.addEventListener("click",closeEmojiOnce),0);
  setTimeout(()=>{const s=document.getElementById("emoji-search");if(s)s.focus();},30);
}
function closeEmojiOnce(){closeEmoji();document.removeEventListener("click",closeEmojiOnce);}
function closeEmoji(){const p=document.getElementById("emoji-pop");if(p)p.style.display="none";}
function renderEmoji(q){
  const pop=document.getElementById("emoji-pop");if(!pop)return;
  q=(q||"").trim().toLowerCase();
  const tabs=EMOJI_CATS.map((c,i)=>`<button class="etab${i===EMOJI_TARGET.cat?' on':''}" title="${c.n}" onclick="EMOJI_TARGET.cat=${i};renderEmoji(document.getElementById('emoji-search').value)"><i class="ti ti-${c.i}"></i></button>`).join("");
  let list;
  if(q)list=EMOJI_CATS.flatMap(c=>c.e).filter((e,i,a)=>a.indexOf(e)===i);
  else list=EMOJI_CATS[EMOJI_TARGET.cat].e;
  const grid=list.map(e=>`<button class="ecell" onclick="pickEmoji('${e}')">${e}</button>`).join("");
  pop.innerHTML=`<div class="etabs">${tabs}</div>
    <div class="esearch"><i class="ti ti-search"></i><input id="emoji-search" placeholder="Search…" autocomplete="off" oninput="renderEmoji(this.value)" onclick="event.stopPropagation()"></div>
    <div class="egrid">${grid}</div>`;
  if(q){const s=document.getElementById("emoji-search");if(s)s.value=q;}
}
function pickEmoji(e){const cb=EMOJI_TARGET.cb;closeEmojiOnce();if(cb)cb(e);}
// Composer: open picker and insert at cursor.
function openComposeEmoji(ev){ev.stopPropagation();
  openEmoji(ev.currentTarget,e=>{
    const inp=document.getElementById("chat-input");const p=inp.selectionStart||inp.value.length;
    inp.value=inp.value.slice(0,p)+e+inp.value.slice(p);const np=p+e.length;inp.setSelectionRange(np,np);inp.focus();
  });
}
// ── Reactions ───────────────────────────────────────────────────────────────
function reactionsHtml(m){
  if(!m.reactions||!m.reactions.length)return"";
  const me=(ME&&ME.username)||"user";
  return `<div class="reactions">`+m.reactions.map(r=>{
    const mine=(r.users||[]).includes(me);
    const who=(r.users||[]).map(memberName).join(", ");
    return `<button class="react${mine?' on':''}" title="${esc(who)}" onclick="toggleReact('${esc(m.id)}','${esc(r.emoji)}')">${r.emoji} <span>${(r.users||[]).length}</span></button>`;
  }).join("")+`</div>`;
}
function openReactPicker(ev,id){ev.stopPropagation();openEmoji(ev.currentTarget,e=>toggleReact(id,e));}
function toggleReact(id,emoji){
  fetch("/api/chat/react",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({id,emoji})}).catch(()=>{});
}
// Both names are used across the message renderers (channel, thread, pins).
const reactTo=toggleReact;
// Apply a reaction event from the socket to the in-memory list + re-render.
function applyReaction(ev){
  const m=CHAT.find(x=>x.id===ev.id);if(m){m.reactions=ev.reactions||[];if(isChatMode()&&ev.channel===CURCHAN)renderChatList(true);}
}
// ── Incoming webhooks ───────────────────────────────────────────────────────
async function openWebhooks(){
  document.getElementById("wh-chan").textContent="#"+chanDisplay(currentChannel());
  document.getElementById("wh-label").value="";
  await loadWebhooks();
  document.getElementById("ov-webhooks").classList.add("open");
}
async function loadWebhooks(){
  let hooks=[];
  try{hooks=await(await fetch("/api/chat/webhooks?channel="+encodeURIComponent(CURCHAN))).json();}catch(e){}
  const box=document.getElementById("wh-list");
  box.innerHTML=hooks.length?hooks.map(w=>{
    const url=location.origin+"/api/chat/hook/"+w.token;
    return `<div class="whrow"><div class="whmeta"><div class="whnm"><i class="ti ti-webhook"></i> ${esc(w.label)}</div><div class="whurl" title="${esc(url)}">${esc(url)}</div></div>
      <button class="memic" title="Copy URL" onclick="copyText('${esc(url)}')"><i class="ti ti-copy"></i></button>
      <button class="memic" title="Revoke" onclick="revokeWebhook('${esc(w.token)}')"><i class="ti ti-trash"></i></button></div>`;
  }).join(""):'<div class="empty">No webhooks yet.</div>';
}
async function createWebhook(){
  const label=document.getElementById("wh-label").value.trim()||"Webhook";
  try{
    const r=await fetch("/api/chat/webhooks",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({channel:CURCHAN,label})});
    if(!r.ok){toasty(await r.text()||"could not create","err");return;}
    const w=await r.json();
    document.getElementById("wh-label").value="";
    await loadWebhooks();
    copyText(location.origin+w.url);toasty("Webhook created — URL copied","ok");
  }catch(e){toasty("could not create","err");}
}
async function revokeWebhook(token){
  try{await fetch("/api/chat/webhooks/"+encodeURIComponent(token),{method:"DELETE"});loadWebhooks();}catch(e){}
}
function copyText(t){navigator.clipboard.writeText(t).then(()=>toasty("Copied","ok")).catch(()=>{});}
// Render a message body with Slack-style formatting: fenced/inline code,
// **bold**, *italic*, links, and @mentions. HTML is escaped; code is verbatim.
function formatMsg(raw){
  if(!raw)return"";
  const blocks=[],inline=[];
  let s=raw.replace(/```([\s\S]*?)```/g,(_,c)=>{blocks.push('<pre class="codeblock">'+esc(c.replace(/^\n/,"").replace(/\n$/,""))+'</pre>');return "\x00"+(blocks.length-1)+"\x00";});
  s=s.replace(/`([^`\n]+)`/g,(_,c)=>{inline.push('<code class="inlinecode">'+esc(c)+'</code>');return "\x01"+(inline.length-1)+"\x01";});
  s=esc(s);
  s=s.replace(/\*\*([^*\n]+)\*\*/g,"<b>$1</b>").replace(/(^|[^*\w])\*([^*\n]+)\*/g,"$1<i>$2</i>");
  s=s.replace(/~~([^~\n]+)~~/g,"<del>$1</del>");
  s=s.replace(/(https?:\/\/[^\s<]+)/g,'<a href="$1" target="_blank" rel="noopener">$1</a>');
  s=s.replace(/@([a-zA-Z0-9._-]+)/g,(mm,u)=>{const known=(MEMBERS||[]).some(x=>x.username.toLowerCase()===u.toLowerCase());
    if(!known)return mm;
    const mine=ME&&ME.username&&ME.username.toLowerCase()===u.toLowerCase();
    return `<span class="mention${mine?' me':''}">@${esc(u)}</span>`;});
  s=s.replace(/\x01(\d+)\x01/g,(_,i)=>inline[+i]).replace(/\x00(\d+)\x00/g,(_,i)=>blocks[+i]);
  return s;
}
function channelName(id){const c=CHANNELS.find(x=>x.id===id);return c?c.name:id;}
function totalUnread(){return Object.values(UNREAD).reduce((a,b)=>a+b,0);}
function clearUnread(chan){if(UNREAD[chan]){delete UNREAD[chan];updateTitle();renderChannels();updateModeBadge();}}
function updateTitle(){
  const n=totalUnread();
  const base=document.title.replace(/^\(\d+\)\s*/,"");
  document.title=(n>0&&document.hidden)?`(${n}) ${base}`:base;
}
window.centerContent = function centerContent(){
  const content=document.querySelector('.content');
  if(!content)return;
  const vw=window.innerWidth;
  if(vw<=900){content.style.width='';content.style.marginLeft='';return;}
  const sideW=246;
  const available=vw-sideW;
  const cols={2000:3800,1800:2800,1600:2200,1440:1600,1340:0};
  let maxW=1340;
  for(const[w,bp]of Object.entries(cols)){
    if(vw>=parseInt(bp)){maxW=parseInt(w);break;}
  }
  const w=Math.min(maxW,available-20);
  const ml=Math.max(0,(available-w)/2);
  content.style.width=w+'px';
  content.style.marginLeft=ml+'px';
}
window.addEventListener('resize',centerContent);
document.addEventListener('visibilitychange',()=>{setTimeout(centerContent,50)});
// Notification transport. Inside the macOS shell (WKWebView) the web
// Notification API doesn't exist, so we bridge to the native app, which posts a
// real macOS notification. In a normal browser we use the Notification API.
function nativeNotify(){return (window.webkit&&window.webkit.messageHandlers&&window.webkit.messageHandlers.coxnotify)||null;}
function notifSupported(){return !!nativeNotify()||("Notification"in window);}
function notifGranted(){return nativeNotify()?NOTIF_ON:(("Notification"in window)&&Notification.permission==="granted");}
// Called by the native shell when the user clicks a macOS notification.
window.__coxOpenChannel=function(id){try{focusChannel(id);}catch(_){}}
// Notify about a message the user isn't actively watching. An in-app toast
// ALWAYS shows (the reliable, permission-free signal); on top of that, when
// notifications are enabled we also raise an OS-level notification + chime.
function notifyMessage(chan,m){
  if(isMuted(chan))return; // this channel's bell is off
  const priv=chan!=="general";
  const title=(priv?"🔒 ":"# ")+channelName(chan)+" · "+(m.user||"someone");
  const body=m.body||(m.attachments&&m.attachments.length?"📎 sent an attachment":"");
  chatToast(chan,m,title);
  const nn=nativeNotify();
  if(nn){
    // Hand off to the macOS shell; it owns the native notification + click.
    try{nn.postMessage({title,body,channel:chan});}catch(_){}
  }else if(("Notification"in window)&&Notification.permission==="granted"){
    try{
      const n=new Notification(title,{body,tag:"cox-chat-"+chan,renotify:true,silent:false});
      n.onclick=()=>{window.focus();focusChannel(chan);n.close();};
    }catch(_){}
  }
  chime();
}
// A clickable in-app notification toast (works with no OS permission).
function chatToast(chan,m,title){
  let box=document.getElementById("chat-toasts");
  if(!box){box=document.createElement("div");box.id="chat-toasts";document.body.appendChild(box);}
  const el=document.createElement("div");
  el.className="ctoast";
  const preview=(m.body||"📎 sent an attachment").slice(0,140);
  el.innerHTML=`<div class="ctoast-h"><span>${esc(title)}</span><i class="ti ti-x ctoast-x"></i></div><div class="ctoast-b">${esc(preview)}</div>`;
  el.querySelector(".ctoast-x").onclick=(e)=>{e.stopPropagation();el.remove();};
  el.onclick=()=>{focusChannel(chan);el.remove();};
  box.appendChild(el);
  while(box.children.length>4)box.firstChild.remove();
  setTimeout(()=>{el.classList.add("out");setTimeout(()=>el.remove(),320);},6000);
}
// Ask for notification permission once (browser). The macOS shell prompts on
// first delivery, so nothing to do there.
function ensureNotifPermission(){
  if(nativeNotify())return;
  if(("Notification"in window)&&Notification.permission==="default"){
    Notification.requestPermission().then(()=>updateChannelBell()).catch(()=>{});
  }
}
let _actx=null;
function chime(){
  try{_actx=_actx||new(window.AudioContext||window.webkitAudioContext)();
    const o=_actx.createOscillator(),g=_actx.createGain();
    o.type="sine";o.frequency.value=660;g.gain.value=0.05;
    o.connect(g);g.connect(_actx.destination);
    o.start();g.gain.exponentialRampToValueAtTime(0.0001,_actx.currentTime+0.25);
    o.stop(_actx.currentTime+0.26);
  }catch(_){}
}
// Per-channel mute (the bell next to Members toggles it for the current channel).
let MUTED=loadMuted();
function loadMuted(){try{return new Set(JSON.parse(localStorage.getItem("cox_mute")||"[]"));}catch(e){return new Set();}}
function saveMuted(){localStorage.setItem("cox_mute",JSON.stringify([...MUTED]));}
function isMuted(chan){return MUTED.has(chan);}
function toggleChannelMute(){
  if(isMuted(CURCHAN)){MUTED.delete(CURCHAN);saveMuted();ensureNotifPermission();toasty("Notifications on for this channel","ok");}
  else{MUTED.add(CURCHAN);saveMuted();toasty("Muted this channel","ok");}
  updateChannelBell();
}
function updateChannelBell(){
  const b=document.getElementById("chat-bell");if(!b)return;
  const muted=isMuted(CURCHAN);
  b.innerHTML=`<i class="ti ti-bell${muted?'-off':''}"></i>`;
  b.classList.toggle("muted",muted);
  b.title=muted?"Channel muted — click to unmute":"Notifications on — click to mute";
}
// Keep the tab title honest when focus changes.
document.addEventListener("visibilitychange",()=>{if(!document.hidden){updateTitle();if(isChatMode())clearUnread(CURCHAN);}});
window.addEventListener("focus",()=>{updateTitle();if(isChatMode())clearUnread(CURCHAN);});
function chatKey(m){return (m.at||"")+"|"+(m.channel||"general")+"|"+(m.user||"")+"|"+(m.body||"");}
function chatHas(m){const k=chatKey(m);return CHAT.some(x=>chatKey(x)===k);}
// Fallback path: merge chat carried on the 1s SSE snapshot into the live list,
// deduped by key. Runs when the WebSocket is down (WKWebView) or when another
// hub wrote to the shared state file. Keeps chronological order by timestamp.
function openChatWS(){
  // Reuse a live socket for the same project; otherwise (re)connect.
  if(CHATWS&&CHATWS_PID===PID&&CHATWS.readyState<=1)return;
  closeChatWS();
  const proto=location.protocol==="https:"?"wss":"ws";
  const pid=PID;CHATWS_PID=pid;
  let ws;try{ws=new WebSocket(`${proto}://${location.host}/api/chat/ws`);}catch(e){return;}
  CHATWS=ws;
  // On (re)connect, resync the open channel so nothing sent during a gap is lost.
  ws.onopen=()=>{if(CHATWS===ws){setChatConn(true);if(isChatMode())loadChatHistory();}};
  ws.onmessage=e=>{try{const m=JSON.parse(e.data);
    if(m.type==="signal"){onSignal(m);return;}
    if(m.type==="reaction"){applyReaction(m);return;}
    // The socket only carries channels this user may see (enforced server-side).
    onChatIncoming(m);}catch(_){}};
  ws.onclose=()=>{if(CHATWS===ws){CHATWS=null;setChatConn(false);
    // Keep the socket alive for the whole session (any page), so notifications
    // arrive even when the user isn't on the Chat view. Reconnect while the
    // same project is active.
    if(PID===pid){clearTimeout(chatwsRetry);chatwsRetry=setTimeout(openChatWS,1500);}}};
  ws.onerror=()=>{try{ws.close();}catch(_){}};
  startChatPoll();
}
// Fallback for environments where the WebSocket is flaky (e.g. some WKWebViews):
// while the socket isn't OPEN, refresh the current channel every few seconds so
// messages never appear "stuck". No-op when the socket is healthy.
let chatPoll=null;
function startChatPoll(){
  if(chatPoll)return;
  chatPoll=setInterval(()=>{
    if(isChatMode()&&!(CHATWS&&CHATWS.readyState===1))loadChatHistory();
  },2500);
}
function closeChatWS(){clearTimeout(chatwsRetry);if(CHATWS){try{CHATWS.onclose=null;CHATWS.close();}catch(_){}}CHATWS=null;}
// Show a subtle "reconnecting" banner while the chat socket is down. Only ever
// appears after a live socket drops — the first connect stays silent.
function setChatConn(ok){const el=document.getElementById("chat-conn");if(el)el.style.display=ok?"none":"flex";}
function renderChatList(force){
  const box=document.getElementById("chat-msgs");if(!box)return;
  const msgs=CHAT;const me=(ME&&ME.username)||"";
  if(!force&&box.dataset.sig===msgs.length)return;
  const atBottom=box.scrollHeight-box.scrollTop-box.clientHeight<90;
  box.dataset.sig=msgs.length;
  if(!msgs.length){box.innerHTML=CHAT_LOAD_ERR
    ? '<div class="chatempty"><i class="ti ti-plug-connected-x" style="color:var(--amber)"></i><div>Couldn\'t load messages</div><span>Check your connection — <a onclick="loadChatHistory()" style="color:var(--accent2);cursor:pointer;text-decoration:underline">retry</a>.</span></div>'
    : '<div class="chatempty"><i class="ti ti-message-circle-2"></i><div>No messages yet</div><span>Say hello to your teammates.</span></div>';
    return;}
  let lastDay="",html="";
  let prev=null;
  msgs.forEach(m=>{
    // Deleted messages always render as a tombstone (was inconsistent before:
    // shown on forced refresh, silently dropped on the next live render).
    if(m.thread_id)return; // thread replies live in the thread panel, not the channel
    const t=m.at||"";const day=t.slice(0,10);
    if(day&&day!==lastDay){lastDay=day;html+=`<div class="chatday"><span>${esc(dayLabel(day))}</span></div>`;prev=null;}
    // Group consecutive messages from same user within 5 min
    m.grouped=!!prev&&prev.user===m.user&&(new Date(m.at)-new Date(prev.at))<300000;
    html+=renderOneMsg(m,"chat");prev=m;
  });
  box.innerHTML=html;
  if(atBottom)box.scrollTop=box.scrollHeight;
  loadPins();renderTyping();
}
// ── Profiles: avatars + Slack-style status ──────────────────────────────────
let PROFILES={};
async function loadProfiles(){try{PROFILES=await(await fetch("/api/profiles")).json();}catch(e){}
  renderDMList();try{renderMe();}catch(e){}}
function avat(u,cls){const p=PROFILES[u]||{};const col=userColor(u);
  // A broken or degenerate image (a 1x1 pixel scaled to fill) renders as a
  // solid black square that looks like a bug in the app. Fall back to the
  // initials tile, which is always readable.
  if(p.avatar)return `<img class="${cls} avimg" src="${esc(p.avatar)}" alt="" data-u="${esc(u)}"
    onerror="avatarFallback(this)" onload="if(this.naturalWidth<8||this.naturalHeight<8)avatarFallback(this)">`;
  return `<div class="${cls}" style="background:${col}26;color:${col}">${esc((memberName(u)||"?").slice(0,2).toUpperCase())}</div>`;}
// Replace an unusable avatar image with its initials tile in place.
function avatarFallback(img){
  const u=img.getAttribute("data-u")||"";
  const col=userColor(u);
  const cls=(img.className||"").replace("avimg","").trim();
  const el=document.createElement("div");
  el.className=cls;
  el.style.background=col+"26";el.style.color=col;
  el.textContent=(memberName(u)||u||"?").slice(0,2).toUpperCase();
  img.replaceWith(el);
}
function statusChip(u){const p=PROFILES[u]||{};
  return p.status_emoji?`<span class="ustatus" title="${esc(p.status_text||"")}">${esc(p.status_emoji)}</span>`:"";}
const STATUS_PRESETS=[["🎯","Focusing"],["🍜","Lunch"],["🏠","WFH"],["📅","In a meeting"],["🌴","Vacation"],["🤒","Sick"]];
setInterval(()=>{loadProfiles();loadMembers();},60000); // teammates' avatar/status/name refresh
function profileTab(t){
  // Never leave every pane hidden: an unknown tab falls back to Status.
  const tabs=["status","profile","prefs"];
  if(!tabs.includes(t))t="status";
  for(const k of tabs){
    document.getElementById("pt-"+k).classList.toggle("on",k===t);
    document.getElementById("pp-"+k).hidden=k!==t;
  }
}
function openProfile(tab){const me=(ME&&ME.username)||"";const p=PROFILES[me]||{};
  document.getElementById("pf-av").innerHTML=avat(me,"pf-avbig");
  document.getElementById("pf-name-hd").textContent=(ME&&(ME.name||ME.username))||me;
  document.getElementById("pf-role-hd").textContent=(ME&&ME.role)||"";
  document.getElementById("pf-emoji").textContent=p.status_emoji||"😊";
  document.getElementById("pf-text").value=p.status_text||"";
  document.getElementById("pf-presets").innerHTML=STATUS_PRESETS.map(([e,t])=>
    `<button class="pf-preset" onclick="pickPreset('${e}','${t}')">${e} ${t}</button>`).join("");
  document.getElementById("pp-rmphoto").hidden=!p.avatar;
  document.getElementById("pp-name").value=(ME&&ME.name)||"";
  document.getElementById("pp-email").value=(ME&&ME.email)||"";
  document.getElementById("pp-pw").value="";
  document.getElementById("ppf-desktop").value=localStorage.getItem("cox_desktop")||"on";
  document.getElementById("ppf-sound").value=localStorage.getItem("cox_sound")||"on";
  profileTab(tab||"status");
  document.getElementById("ov-profile").classList.add("open");}
// Slack behaviour: tapping a preset SETS the status immediately.
async function pickPreset(emoji,text){
  document.getElementById("pf-emoji").textContent=emoji;
  document.getElementById("pf-text").value=text;
  await saveStatus(false);
}
async function savePopupProfile(){
  const n=document.getElementById("pp-note");
  const pw=document.getElementById("pp-pw").value;
  try{const r=await fetch("/api/auth/profile",{method:"PATCH",headers:{"Content-Type":"application/json"},
      body:JSON.stringify({name:document.getElementById("pp-name").value,email:document.getElementById("pp-email").value,password:pw||undefined})});
    if(r.ok){n.innerHTML='<span style="color:var(--green)">Saved</span>';try{ME=await(await fetch("/api/auth/me")).json();renderUserBadge&&renderUserBadge();loadMembers();}catch(e){}}
    else n.innerHTML=`<span style="color:var(--red)">${esc(await r.text())}</span>`;
  }catch(e){n.innerHTML='<span style="color:var(--red)">Network error</span>';}}
function savePopupPrefs(){
  localStorage.setItem("cox_desktop",document.getElementById("ppf-desktop").value);
  localStorage.setItem("cox_sound",document.getElementById("ppf-sound").value);
  toasty("Preferences saved","ok");}
async function removeAvatar(){
  try{const r=await fetch("/api/profile/avatar",{method:"DELETE"});
    if(r.ok){await loadProfiles();try{renderMe();}catch(e){}openProfile("profile");toasty("Photo removed","ok");renderChatList(true);}
    else toasty(await r.text()||"Could not remove","err");
  }catch(e){toasty("Network error","err");}}
async function uploadAvatar(input){const f=input.files[0];input.value="";if(!f)return;
  if(f.size>2*1024*1024){toasty("Ảnh tối đa 2MB","err");return;}
  const fd=new FormData();fd.append("file",f);
  try{const r=await fetch("/api/profile/avatar",{method:"POST",body:fd});
    if(r.ok){await loadProfiles();try{renderMe();}catch(e){}openProfile("profile");toasty("Avatar updated","ok");renderChatList(true);}
    else toasty(await r.text()||"Upload failed","err");
  }catch(e){toasty("Network error","err");}}
// NOTE: named saveStatus — `saveProfile` is taken by the account-settings form.
async function saveStatus(clear){
  const emoji=clear?"":document.getElementById("pf-emoji").textContent.trim();
  const text=clear?"":document.getElementById("pf-text").value.trim();
  try{const r=await fetch("/api/profile",{method:"POST",headers:{"Content-Type":"application/json"},
      body:JSON.stringify({status_emoji:emoji==="😊"&&!text?"":emoji,status_text:text})});
    if(r.ok){await loadProfiles();close_("ov-profile");toasty(clear?"Status cleared":"Status saved","ok");}
    else toasty(await r.text()||"Could not save","err");
  }catch(e){toasty("Network error","err");}}
// "Today" / "Yesterday" / a friendly local date for the channel day divider.
function dayLabel(day){const d=new Date(day+"T12:00:00");const now=new Date();
  const today=now.toISOString().slice(0,10),y=new Date(now-86400000).toISOString().slice(0,10);
  if(day===today)return "Today"; if(day===y)return "Yesterday";
  return d.toLocaleDateString([],{weekday:"long",month:"long",day:"numeric"});}
// Deterministic per-user accent so each teammate keeps a stable bubble color.
function userColor(u){const pal=["#38bdf8","#a78bfa","#f472b6","#34d399","#fbbf24","#fb7185","#60a5fa","#c084fc"];
  let h=0;for(let i=0;i<(u||"").length;i++)h=(h*31+u.charCodeAt(i))>>>0;return pal[h%pal.length];}
// Wrap the chat input's selection in a markdown marker (bold/italic/code).
function chatWrap(mk){const inp=document.getElementById("chat-input");if(!inp)return;
  const a=inp.selectionStart||0,b=inp.selectionEnd||0,v=inp.value;const sel=v.slice(a,b)||"text";
  inp.value=v.slice(0,a)+mk+sel+mk+v.slice(b);inp.focus();
  inp.setSelectionRange(a+mk.length,a+mk.length+sel.length);}
function sendChat(){const inp=document.getElementById("chat-input");const body=inp.value.trim();
  const attachments=(ATT.chat||[]).slice();
  if(!body&&!attachments.length)return;
  const channel=CURCHAN;
  const payload=JSON.stringify({body,channel,attachments});
  // Clear the composer optimistically, but keep the text so we can put it back
  // verbatim if the send fails — otherwise a dropped message is unrecoverable.
  const restore=()=>{if(!inp.value)inp.value=body;ATT.chat=attachments;renderAttStrip("chat");inp.focus();};
  inp.value="";ATT.chat=[];renderAttStrip("chat");
  // Prefer the live socket (the server echoes the canonical message back). Fall
  // back to REST whenever the socket isn't OPEN or the frame can't be queued.
  let viaWs=false;
  if(CHATWS&&CHATWS.readyState===1){try{CHATWS.send(payload);viaWs=true;}catch(_){}}
  if(!viaWs){
    fetch("/api/chat/send",{method:"POST",headers:{"Content-Type":"application/json"},body:payload})
      .then(r=>{if(!r.ok)throw new Error(r.status);return loadChatHistory();})
      .catch(()=>{restore();toasty("Couldn't send — check your connection and try again.","err");});
  }
  setTimeout(scrollChatToBottom, 100);}
function scrollChatToBottom(){const box=document.getElementById("chat-msgs");if(box){box.scrollTop=box.scrollHeight;chatScrollSpy();}}
function chatScrollSpy(){const box=document.getElementById("chat-msgs");if(!box)return;const btn=document.getElementById("chat-scroll-btn");const dist=box.scrollHeight-box.scrollTop-box.clientHeight;btn.classList.toggle("show",dist>120);}
// ── Shared attachment upload + rendering (chat + discussion) ────────────────
let ATT={chat:[],disc:[]};
async function uploadOne(file,surface){const fd=new FormData();fd.append("file",file);
  // Chat is hub-wide (system media); discussion uploads stay per-project.
  const url=surface==="chat"?"/api/chat/upload":api("/upload");
  const r=await fetch(url,{method:"POST",body:fd});
  if(!r.ok)throw new Error(await r.text()||"upload failed");
  return await r.json();}
function pickFiles(surface,input){handleFiles(surface,[...input.files]);input.value="";}
function dropFiles(surface,ev){ev.preventDefault();ev.currentTarget.classList.remove("dropping");
  handleFiles(surface,[...(ev.dataTransfer.files||[])]);}
async function handleFiles(surface,files){
  for(const f of files){
    if(f.size>25*1024*1024){toasty(f.name+" too large (max 25MB)","err");continue;}
    const placeholder={name:f.name,uploading:true};ATT[surface]=ATT[surface]||[];ATT[surface].push(placeholder);renderAttStrip(surface);
    try{const att=await uploadOne(f,surface);const i=ATT[surface].indexOf(placeholder);if(i>=0)ATT[surface][i]=att;else ATT[surface].push(att);}
    catch(e){const i=ATT[surface].indexOf(placeholder);if(i>=0)ATT[surface].splice(i,1);toasty("Upload failed: "+f.name,"err");}
    renderAttStrip(surface);
  }
}
function removeAtt(surface,idx){ATT[surface].splice(idx,1);renderAttStrip(surface);}
function renderAttStrip(surface){const el=document.getElementById(surface+"-att");if(!el)return;
  const a=ATT[surface]||[];el.innerHTML=a.map((x,i)=>`<div class="att-chip${x.uploading?' up':''}">
    ${x.mime&&x.mime.startsWith("image/")?`<img src="${esc(x.url)}">`:`<i class="ti ti-file"></i>`}
    <span class="att-nm">${esc(x.name)}</span>${x.uploading?'<i class="ti ti-loader-2 att-spin"></i>':`<i class="ti ti-x att-rm" onclick="removeAtt('${surface}',${i})"></i>`}</div>`).join("");
  el.style.display=a.length?"flex":"none";}
// Render a message's attachments: images inline (click to open), files as chips.
function attHtml(atts){if(!atts||!atts.length)return"";
  return `<div class="msg-att">`+atts.map(a=>{
    if(a.mime&&a.mime.startsWith("image/"))return `<a href="${esc(a.url)}" target="_blank" rel="noopener" class="msg-img"><img src="${esc(a.url)}" alt="${esc(a.name)}" loading="lazy"></a>`;
    return `<a href="${esc(a.url)}" target="_blank" rel="noopener" download class="msg-file"><i class="ti ti-file-download"></i> <span>${esc(a.name)}</span> <em>${fmtBytes(a.size||0)}</em></a>`;
  }).join("")+`</div>`;}
// Team-health metrics computed from the live state — data-driven Scrum.
function renderHealth(s){
  const el=document.getElementById("ov-health"); if(!el)return;
  const t=s.tickets||[];
  const isDone=x=>["done","documented","verified"].includes((x.status||"").toLowerCase());
  const reviews=s.reviews||[];
  const appr=reviews.filter(r=>(r.decision||"")==="approve").length;
  const chg=reviews.filter(r=>(r.decision||"")==="request_changes").length;
  const reviewTotal=appr+chg;
  const rejectRate=reviewTotal?Math.round(chg*100/reviewTotal):0;
  // Sprint velocity: committed vs shipped this sprint.
  const sp=s.sprint; let velo="—", veloSub="no sprint";
  if(sp&&sp.committed){const done=sp.committed.filter(id=>t.some(x=>x.id===id&&isDone(x))).length;
    velo=`${done}/${sp.committed.length}`; veloSub=`sprint #${sp.number}`;}
  const openBugs=t.filter(x=>(x.type||x.ticket_type||"").toLowerCase()==="bug"&&(x.status||"").toLowerCase()==="open").length;
  const wip=t.filter(x=>(x.status||"").toLowerCase()==="inprogress").length;
  const refactors=t.filter(x=>(x.title||"").startsWith("Refactor:")&&!isDone(x)).length;
  const mem=((s.decisions||[]).length)+((s.lessons||[]).length);
  const card=(lbl,val,sub,col)=>`<div class="hcard"><div class="hval" style="color:${col||'var(--text)'}">${val}</div><div class="hlbl">${lbl}</div>${sub?`<div class="hsub">${sub}</div>`:''}</div>`;
  el.innerHTML=`<div class="sec" style="margin-top:20px"><i class="ti ti-heart-rate-monitor" style="color:var(--accent2)"></i> Team health</div>
    <div class="hgrid">
      ${card("Sprint velocity",velo,veloSub,"var(--green)")}
      ${card("WIP (in progress)",wip,wip>6?'high — cân nhắc giảm':'ok',wip>6?'var(--amber)':'var(--text)')}
      ${card("Open bugs",openBugs,"chưa fix",openBugs>4?'var(--red)':'var(--text)')}
      ${card("PR reject rate",reviewTotal?rejectRate+'%':'—',`${appr}✓ / ${chg}✗`,rejectRate>50?'var(--amber)':'var(--text)')}
      ${card("Refactor debt",refactors,"ticket refactor mở",refactors>0?'var(--amber)':'var(--text)')}
      ${card("Team memory",mem,"decisions + lessons","var(--accent2)")}
    </div>`;
}
function render(s){STATE=s;renderSidebar(s);renderActive();ingestChatSnapshot(s.chat);renderEngineAlert(s);}
// A stopped team looks like a broken team until you know the engine is down.
// Turn a raw backend error into one calm human sentence + the kind of trouble
// it is, so the banner can be a quiet pill instead of a red slab dumping a
// stack of "SA: backend failure: ... API Error: 401 ...". Kinds: "auth"
// (recoverable by re-login), "quota" (clears itself / raise cap), "slow"
// (transient), "other".
function engineIncidentKind(reason){
  const r=(reason||"").toLowerCase();
  if(/revoked|oauth|session expired|token expired|expired token|authenticat|unauthorized|401|403/.test(r))return "auth";
  if(/quota|spend limit|usage limit|rate.?limit|429|billing|credit|insufficient|out of tokens|overloaded|529/.test(r))return "quota";
  if(/tim(e|ed) ?out|timeout|deadline|connection|stream closed|broken pipe|reset by peer|temporarily/.test(r))return "slow";
  return "other";
}
function engineIncidentSay(engine,kind){
  switch(kind){
    case "auth":  return `${engine} sign-in expired — refresh it: <code>${engine} login</code>`;
    case "quota": return `${engine} hit its usage limit — clears on reset, or raise the cap`;
    case "slow":  return `${engine} slow to respond — retrying on its own`;
    default:      return `${engine} had a run error`;
  }
}
function renderEngineAlert(s){
  const el=document.getElementById("engine-alert");if(!el)return;
  const inc=(s&&s.engine_incidents)||[];
  if(!inc.length){el.hidden=true;el.innerHTML="";return;}
  el.hidden=false;
  // Quiet by design: a slim pill with a status dot and one plain sentence.
  // The raw backend string lives on hover for whoever wants it — the surface
  // says what happened and what clears it, nothing more. A banner that shouts
  // gets tuned out; the point is a glance, not an alarm.
  el.innerHTML=inc.map(i=>{
    const kind=engineIncidentKind(i.reason);
    const say=engineIncidentSay(esc(i.engine),kind);
    const meta=kind==="auth"?"needs re-login":"clears itself";
    return `<div class="eng-alert eng-${kind}" title="${esc(i.reason)}">
      <span class="eng-dot"></span>
      <span class="eng-say">${say}</span>
      <span class="eng-meta">${i.hits} run${i.hits>1?"s":""} · ${meta}</span>
    </div>`;
  }).join("");
}
function depChips(ids){
  if(!ids||!ids.length)return '<span style="color:var(--dim)">—</span>';
  return ids.map(id=>{const dt=(STATE.tickets||[]).find(x=>x.id===id);
    const done=dt&&(dt.status==="done"||dt.status==="documented");
    const col=done?"var(--green)":(dt?"var(--amber)":"var(--dim)");
    return `<span onclick="showTicket('${id}')" title="${dt?esc(dt.title)+' · '+esc(dt.status):'unknown'}" style="cursor:pointer;display:inline-flex;align-items:center;gap:4px;font-size:11px;padding:3px 8px;border-radius:7px;background:${col}22;color:${col};margin-right:5px">
      <i class="ti ti-${done?'check':'circle'}" style="font-size:11px"></i>${esc(id)}</span>`;}).join("");}
async function showTicket(id){
  // Full detail (incl. design specs stripped from list payloads) loads on demand.
  const body=document.getElementById("ticket-body");
  body.innerHTML='<span class="x" onclick="close_(\'ov-ticket\')"><i class="ti ti-x"></i></span><div class="empty">loading…</div>';
  document.getElementById("ov-ticket").classList.add("open");
  let t=null;try{t=await(await fetch(api("/ticket/"+encodeURIComponent(id)))).json();}catch(e){}
  if(!t||!t.id){t=(STATE.tickets||[]).find(x=>x.id===id);}
  if(!t){body.innerHTML='<span class="x" onclick="close_(\'ov-ticket\')"><i class="ti ti-x"></i></span><div class="empty">ticket not found</div>';return;}
  const d=t.design||{},tech=d.technical,ux=d.ux,ac=t.acceptance_criteria||[];
  let h=`<span class="x" onclick="close_('ov-ticket')"><i class="ti ti-x"></i></span><h3>${esc(t.title)}</h3>
    <div class="msub">${esc(t.id)} · ${esc(t.type)}</div>
    <div class="mrow"><span class="lbl">Status</span><b>${esc(t.status)}</b></div>
    <div class="mrow"><span class="lbl">Priority</span>
      <div style="display:flex;gap:6px;align-items:center">
        ${["high","medium","low"].map(p=>`<span onclick="setPriority('${t.id}','${p}')" style="cursor:pointer;font-size:11px;padding:3px 10px;border-radius:7px;font-weight:600;${t.priority===p?`background:var(--accentbg);color:var(--accent2)`:'background:var(--card2);color:var(--muted)'}">${p}</span>`).join("")}
        <span style="color:var(--dim);font-size:12px;margin-left:6px">· ${esc(t.complexity)}${t.has_ui?' · UI':''}</span></div></div>
    <div class="mrow"><span class="lbl">Assignee</span>
      <div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap">
        ${t.assignee?`<span class="tk" style="background:var(--accentbg);color:var(--accent2)"><i class="ti ti-user"></i> @${esc(t.assignee)}</span>
          <button class="tk-btn" style="padding:4px 10px;font-size:11px" onclick="assignTicket('${t.id}','')"><i class="ti ti-robot"></i> Return to agents</button>`
        :`<span style="color:var(--dim);font-size:12px">agents (pool)</span>
          <select id="tk-assign-sel" style="background:var(--card2);color:var(--text);border:1px solid var(--border);border-radius:7px;padding:4px 8px;font-size:12px"><option value="">choose person…</option></select>
          <button class="tk-btn" style="padding:4px 10px;font-size:11px" onclick="assignTicket('${t.id}',document.getElementById('tk-assign-sel').value)"><i class="ti ti-user-plus"></i> Assign</button>`}
      </div></div>
    <div class="mrow"><span class="lbl">Blocked by</span>${depChips(t.depends_on)}</div>
    <div class="mrow"><span class="lbl">Blocks</span>${depChips((STATE.tickets||[]).filter(x=>(x.depends_on||[]).includes(t.id)).map(x=>x.id))}</div>
    <div class="mrow" style="display:block"><span class="lbl">Description</span><div class="doc-body md" style="margin-top:7px;color:var(--muted);line-height:1.6;font-size:13px">${t.description?mdRender(t.description):'—'}</div></div>
    <div class="mrow" style="display:block"><span class="lbl">Acceptance criteria</span>${ac.length?`<div class="aclist">${ac.map(c=>`<div class="acitem"><i class="ti ti-square-check"></i> ${esc(c)}</div>`).join("")}</div>`:'<div style="margin-top:6px;color:var(--dim);font-size:12px">— none defined yet</div>'}</div>`;
  if(tech)h+=`<div class="mrow" style="display:block;border:none"><span class="lbl">Technical spec</span><pre>${esc(tech.approach)}\nfiles: ${esc((tech.files||[]).join(", "))}\napi: ${esc(tech.api_contract)}\ntest: ${esc(tech.test_plan)}</pre></div>`;
  if(ux)h+=`<div class="mrow" style="display:block;border:none"><span class="lbl">UI/UX spec</span><pre>${esc(ux.user_flow)}\nscreens: ${esc((ux.screens||[]).join(", "))}</pre></div>`;
  const canWork=["pending","ready","open"].includes(t.status);
  if(t.cost_hold!=null&&!t.cost_approved)h+=`<div class="mrow" style="display:block;border:1px solid var(--amber);border-radius:9px;padding:10px 12px;background:color-mix(in srgb,var(--amber) 9%,transparent)"><span style="color:var(--amber);font-weight:700"><i class="ti ti-currency-dollar"></i> Held for cost approval</span><div style="font-size:12.5px;color:var(--muted);margin-top:4px">Estimated ~$${(+t.cost_hold).toFixed(2)}/run exceeds the approval gate. Agents will skip this ticket until you approve it.</div><button class="pri" style="margin-top:8px" onclick="approveCost('${t.id}')"><i class="ti ti-check"></i> Approve run</button></div>`;
  h+=`<div class="tk-actions">
    <button class="tk-btn" onclick="editTicket('${t.id}')"><i class="ti ti-edit"></i> Edit</button>
    ${canWork?`<button class="tk-btn go" onclick="workNext('${t.id}')"><i class="ti ti-player-play-filled"></i> Work on this next</button>`:''}
    ${(t.status==="pending"&&tech)?`<button class="tk-btn go" onclick="humanGate('${t.id}','ready')"><i class="ti ti-checks"></i> Approve → Ready</button>`:''}
    ${t.status==="fixed"?`<button class="tk-btn" onclick="inboxSendBack('${t.id}')"><i class="ti ti-arrow-back-up"></i> Send back</button>`:''}
    ${t.status==="fixed"?`<button class="tk-btn go" onclick="humanGate('${t.id}','verify')"><i class="ti ti-shield-check"></i> Mark Verified</button>`:''}
    ${(t.status==="pending"||t.status==="open")?`<button class="tk-btn danger" onclick="rejectTicket('${t.id}')"><i class="ti ti-ban"></i> Reject</button>`:''}
  </div>`;
  h+=`<div class="mrow" style="display:block;border:none;margin-top:6px"><span class="lbl">Comments</span><div id="tk-comments" style="margin-top:8px">${'<div class="empty" style="padding:8px">loading…</div>'}</div>
    <div class="tkc-wrap">${mdToolbar('tkc-input')}<div class="tkc-compose"><input id="tkc-input" placeholder="Add a comment…  (**markdown** · Enter to post · @ to mention)" onkeydown="if(!imeEnter(event)&&event.key==='Enter')postTicketComment('${t.id}')"><button class="pri" onclick="postTicketComment('${t.id}')"><i class="ti ti-send"></i></button></div></div></div>`;
  body.innerHTML=h;
  fillAssignSelect();
  renderTicketComments(t.id);}
async function fillAssignSelect(){
  const sel=document.getElementById("tk-assign-sel");if(!sel)return;
  try{const members=await(await fetch(api("/members"))).json();
    for(const m of (members||[])){const o=document.createElement("option");o.value=m.username||m;o.textContent="@"+(m.username||m);sel.appendChild(o);}
  }catch(e){}}
async function assignTicket(id,user){
  try{const r=await fetch(api("/ticket/"+encodeURIComponent(id)+"/assign"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({username:user||""})});
    if(!r.ok)toasty(await r.text(),"err");else toasty(user?("Assigned to @"+user):"Returned to agents","ok");
  }catch(e){}
  showTicket(id);}
async function humanGate(id,action){
  try{const r=await fetch(api("/ticket/"+encodeURIComponent(id)+"/"+action),{method:"POST",headers:{"Content-Type":"application/json"},body:"{}"});
    if(!r.ok)toasty(await r.text(),"err");else toasty(action==="ready"?(id+" → Ready"):(id+" verified"),"ok");
  }catch(e){}
  showTicket(id);}
async function rejectTicket(id){try{await fetch(api("/ticket/"+id+"/reject"),{method:"POST"});close_("ov-ticket");}catch(e){}}
async function approveCost(id){try{await fetch(api("/ticket/"+id+"/approve-cost"),{method:"POST"});toasty("Approved — agents may run "+id,"ok");showTicket(id);}catch(e){toasty("Approve failed","err");}}
function editTicket(id){const t=(STATE.tickets||[]).find(x=>x.id===id);if(!t)return;
  const body=document.getElementById("ticket-body");
  body.innerHTML=`<span class="x" onclick="showTicket('${id}')"><i class="ti ti-x"></i></span>
    <h3>Edit ${esc(id)}</h3><div class="msub">${esc(t.type)} · ${esc(t.status)}</div>
    <div class="mrow" style="display:block;border:none"><span class="lbl">Title</span>
      <input id="ed-title" class="ed-in" value="${esc(t.title||'')}"></div>
    <div class="mrow" style="display:block;border:none"><span class="lbl">Description</span>
      <textarea id="ed-desc" class="ed-ta" rows="9" placeholder="What & why (markdown ok)…">${esc(t.description||'')}</textarea></div>
    <div class="tk-actions">
      <button class="tk-btn go" onclick="saveTicketEdit('${id}')"><i class="ti ti-check"></i> Save</button>
      <button class="tk-btn" onclick="showTicket('${id}')">Cancel</button></div>`;
  document.getElementById("ed-title").focus();}
async function saveTicketEdit(id){
  const title=(document.getElementById("ed-title").value||"").trim();
  const description=document.getElementById("ed-desc").value||"";
  if(!title){document.getElementById("ed-title").focus();return;}
  try{await fetch(api("/ticket/"+id+"/edit"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({title,description})});}catch(e){}
  showTicket(id);}
async function workNext(id){
  try{await fetch(api("/ticket/"+id+"/priority"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({priority:"high"})});}catch(e){}
  try{await ctl('resume');}catch(e){}
  showTicket(id);}
const TKC_QUICK=["👍","❤️","🎉","🚀","👀","✅"];
async function renderTicketComments(tid){
  const box=document.getElementById("tk-comments");if(!box)return;
  let list=[];try{list=await(await fetch(api("/comments?ticket="+encodeURIComponent(tid)))).json();}catch(e){}
  if(!Array.isArray(list)||!list.length){box.innerHTML='<div class="empty" style="padding:8px;font-size:12px">No comments yet — start the thread.</div>';return;}
  const me=(ME&&ME.username)||"user";
  box.innerHTML=list.map(c=>{
    const mine=c.author==="USER"||c.author===me;
    const av=(c.author||"?").slice(0,2).toUpperCase();
    const chips=(c.reactions||[]).map(r=>{const on=(r.users||[]).includes(me);
      return `<button class="react${on?' on':''}" title="${esc((r.users||[]).join(', '))}" onclick="reactComment('${tid}','${esc(c.id)}','${esc(r.emoji)}')">${r.emoji} <span>${(r.users||[]).length}</span></button>`;}).join("");
    return `<div class="tkc">
      <div class="tkc-av${mine?' me':''}">${esc(av)}</div>
      <div class="tkc-main"><div class="tkc-h"><b>${esc(c.author==='USER'?'You':c.author)}</b> <span class="tkc-t">${esc((c.at||'').slice(0,16).replace('T',' '))}</span></div>
        <div class="tkc-b">${formatMsg(c.body)}</div>
        ${attHtml(c.attachments)}
        <div class="tkc-react">${chips}<button class="tkc-addr" onclick="tkcReactMenu(event,'${tid}','${esc(c.id)}')" title="React"><i class="ti ti-mood-plus"></i></button></div></div>
    </div>`;}).join("");
}
function tkcReactMenu(ev,tid,cid){ev.stopPropagation();
  const old=document.getElementById("tkc-menu");if(old)old.remove();
  const m=document.createElement("div");m.id="tkc-menu";m.className="tkc-menu";
  m.innerHTML=TKC_QUICK.map(e=>`<button onclick="reactComment('${tid}','${cid}','${e}');document.getElementById('tkc-menu').remove()">${e}</button>`).join("");
  document.body.appendChild(m);const r=ev.currentTarget.getBoundingClientRect();
  m.style.left=Math.min(r.left,window.innerWidth-m.offsetWidth-10)+"px";m.style.top=(r.bottom+4)+"px";
  setTimeout(()=>document.addEventListener("click",function h(){m.remove();document.removeEventListener("click",h);},{once:true}),0);
}
async function reactComment(tid,cid,emoji){
  try{await fetch(api("/comments/"+encodeURIComponent(cid)+"/react"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({emoji})});}catch(e){}
  renderTicketComments(tid);
}
async function postTicketComment(tid){
  const inp=document.getElementById("tkc-input");const body=(inp.value||"").trim();if(!body)return;
  inp.value="";inp.disabled=true;
  // A swallowed failure looked exactly like a posted comment: the box cleared
  // and nothing appeared. Say so instead, and give the text back.
  try{
    const r=await fetch(api("/comments"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({body,ticket:tid})});
    if(!r.ok){inp.value=body;toasty(await r.text()||"Comment failed","err");}
  }catch(e){inp.value=body;toasty("Network error — comment not posted","err");}
  inp.disabled=false;await renderTicketComments(tid);inp.focus();
}
// Fields of coxagent.json the hub could not read. It keeps every OTHER
// setting in the file and defaults only these (COX-B050) — which is precisely
// why it has to say so: a deploy.auto_rollback that reverted itself because a
// neighbouring port had one digit too many is invisible otherwise, and this
// screen writes back what it shows, so Save would make the loss permanent.
function cfgHealthBanner(h){
  if(!h)return"";
  const warn=(body)=>`<div class="panel" style="margin-bottom:12px;border-color:var(--amber)">
    <div style="font-size:12px;color:var(--amber)"><i class="ti ti-alert-triangle"></i> ${body}</div></div>`;
  if(h.unreadable)return warn(`<b>coxagent.json could not be read</b> — ${esc(h.unreadable)}.
    These settings are defaults; fix the file on disk, they are NOT what the project is running on.`);
  const d=Array.isArray(h.defects)?h.defects:[];
  if(!d.length)return"";
  return warn(`<b>${d.length} setting${d.length>1?'s':''} in coxagent.json could not be read</b> and ${d.length>1?'are':'is'} showing the default —
    every other setting in the file was kept.
    <ul style="margin:6px 0 0 16px">${d.map(x=>`<li><code>${esc(x.path)}</code> — ${esc(x.reason)}</li>`).join("")}</ul>
    <div style="margin-top:6px">Fix the file on disk to keep ${d.length>1?'those values':'that value'}: saving from this screen writes the defaults over ${d.length>1?'them':'it'}.</div>`);
}
async function loadSettings(){
  if(!canManage()){
    // Member-tier users get self-service settings only: the MCP tab.
    document.getElementById("settings-body").innerHTML=`
      <div class="settabs"><button class="settab-btn on" data-t="mcp"><i class="ti ti-plug-connected"></i> MCP</button></div>
      <div class="settab" data-p="mcp"><div id="mcp-panel"></div></div>`;
    window._setTab="mcp";renderMcpPanel();return;
  }
  let cfg={};try{cfg=await(await fetch(api("/config"))).json();}catch(e){}window._cfg=cfg;
  let cfgHealth=null;try{cfgHealth=await(await fetch(api("/config/defects"))).json();}catch(e){}
  let td={tools:[]};try{td=await(await fetch("/api/tooling")).json();}catch(e){}
  let ga={};try{ga=await(await fetch(api("/git/auth"))).json();}catch(e){}
  const tools=td.tools||[];const missing=tools.filter(t=>!t.present).length;
  const gprov=(cfg.git&&cfg.git.provider)||"github";
  const authRow=ga.present?`<div class="toolrow toolauth">
    <i class="ti ti-${ga.authenticated?'user-check':'user-x'}" style="color:${ga.authenticated?'var(--green)':'var(--amber)'};font-size:17px"></i>
    <div class="toolinfo"><div><b>${esc(ga.tool||gprov)} sign-in</b> <span class="tooldim">· ${ga.authenticated?('connected as '+esc(ga.account||'?')):'not signed in'}</span></div></div>
    ${ga.authenticated?'<span class="toolok">connected</span>':`<button class="gc-btn pri" onclick="openConnect('${gprov}')"><i class="ti ti-plug"></i> Connect</button>`}
    <button class="gc-btn" onclick="testGitConnection(this)" title="repo → remote → reachable → push permission → pull requests (push and PRs use different credentials)"><i class="ti ti-antenna-bars-5"></i> Test connection</button>
  </div><div id="git-test-result"></div>`:'';
  const brewBanner=(td.os==="macos"&&!td.has_brew)
    ? `<div class="toolrow toolbrew"><i class="ti ti-alert-triangle" style="color:var(--amber);font-size:17px"></i>
        <div class="toolinfo"><div><b>Homebrew</b> <span class="tooldim">· the installer these commands use — not found</span></div>
          <div class="toolinstall"><code>${esc(td.brew_install||'')}</code><button class="toolcopy" onclick="copyText(this,'${esc((td.brew_install||'').replace(/'/g,"\\'"))}')" title="Copy"><i class="ti ti-copy"></i></button></div></div></div>` : '';
  const cp=s=>String(s||'').replace(/\\/g,'\\\\').replace(/'/g,"\\'");
  const toolingHtml=`<div class="toolcard">
    <div class="toolhead"><i class="ti ti-tools" style="color:var(--accent2)"></i> Developer tooling <span class="tooldim" style="font-weight:400">· ${esc(td.os||'')}</span> ${missing?`<span class="toolwarn">${missing} missing</span>`:'<span class="toolok">all present</span>'}</div>
    <div class="toolgrid">${brewBanner}${tools.map(t=>`<div class="toolrow">
      <i class="ti ti-${t.present?'circle-check-filled':'circle-x'}" style="color:${t.present?'var(--green)':'var(--red)'};font-size:17px"></i>
      <div class="toolinfo"><div><b>${esc(t.name)}</b> <span class="tooldim">· ${esc(t.purpose)}</span></div>
        ${t.present?`<code class="toolpath">${esc(t.path)}</code>`:`<div class="toolinstall"><code>${esc(t.install)}</code><button class="toolcopy" onclick="copyText(this,'${cp(t.install)}')" title="Copy"><i class="ti ti-copy"></i></button></div>`}
      </div>${t.needs_auth&&!t.present?'<span class="tooltag">needs your login</span>':''}</div>`).join("")}${authRow}</div>
    ${missing?'<div class="toolnote"><i class="ti ti-info-circle"></i> Install the missing tools, then <b>sign them in</b> (e.g. <code>gh auth login</code>) — auth needs you, agents can\'t do it. Restart the app after installing.</div>':''}
  </div>`;
  let detected=[];try{detected=await(await fetch("/api/engines")).json();}catch(e){}
  if(!Array.isArray(detected))detected=[];
  const detNames=new Set(detected.map(d=>d.name));window._detected=detNames;
  if(detNames.has("opencode"))loadOpencodeModels(); // refresh opencode providers
  const def=cfg.engine&&cfg.engine.default||{engine:"claude",model:"sonnet"},per=cfg.engine&&cfg.engine.per_role||{},wf=cfg.workflow||{},pol=cfg.policy||{},git=cfg.git||{};
  const hu=wf.human||{},ad=hu.adaptive||{};
  // Real agent CLIs only. `scripted`/`mock` are offline test engines (no real
  // LLM) — only shown if a project is already pinned to one, never offered new.
  const realEng=["claude","opencode","copilot","hermes","gemini","codex"];
  const eng=[...realEng]; ["scripted","mock"].forEach(t=>{if([def.engine,...Object.values(per).map(p=>p.engine)].includes(t))eng.push(t);});
  const label=e=>e+(detNames.has(e)?" ✓":(e==="scripted"||e==="mock")?" (test)":" (not installed)");
  const opt=(s)=>eng.map(e=>`<option value="${e}" ${e===s?'selected':''}>${label(e)}</option>`).join("");
  const detBanner=(detected&&detected.length)
    ? `<div class="panel" style="margin-bottom:12px"><div style="font-size:12px;color:var(--muted);margin-bottom:6px"><i class="ti ti-cpu" style="color:var(--accent2)"></i> Detected agent CLIs on this machine — agents run locally via these:</div>`
      + detected.map(d=>`<div style="display:flex;align-items:center;gap:8px;font-size:12px;padding:2px 0"><span class="tk" style="background:color-mix(in srgb,var(--green) 13%,transparent);color:var(--green)">${esc(d.name)}</span><span style="color:var(--dim);font-family:ui-monospace,monospace">${esc(d.path)}</span></div>`).join("")
      + `</div>`
    : `<div class="panel" style="margin-bottom:12px"><div style="font-size:12px;color:var(--amber)"><i class="ti ti-alert-triangle"></i> No agent CLI detected on PATH. Install <b>claude</b> or <b>opencode</b> to run real agents (scripted/mock work offline).</div></div>`;
  const defRow=`<div class="fr"><span class="lbl">Default engine</span><select id="eng-default" onchange="onEngine('default')">${opt(def.engine)}</select>
    <span id="mc-default" style="flex:1;display:flex">${modelControl(def.engine,def.model,'default')}</span></div>`;
  const roleRows=ROLES.map(r=>{const c=per[r]||{};return `<div class="fr"><span class="lbl">${r}</span>
    <select id="eng-${r}" onchange="onEngine('${r}')"><option value="">(default)</option>${eng.map(e=>`<option value="${e}" ${c.engine===e?'selected':''}>${label(e)}</option>`).join("")}</select>
    <span id="mc-${r}" style="flex:1;display:flex">${modelControl(c.engine||"",c.model||"",r)}</span></div>`;}).join("");
  const anyOverride=ROLES.some(r=>per[r]&&per[r].engine);
  document.getElementById("settings-body").innerHTML=`
    <datalist id="opencode-models">${OC_MODELS.map(m=>`<option value="${m}">`).join("")}</datalist>
    ${cfgHealthBanner(cfgHealth)}
    <div class="settabs">
      <button class="settab-btn" data-t="engines" onclick="setSetTab('engines')"><i class="ti ti-cpu"></i> Engines</button>
      <button class="settab-btn" data-t="workflow" onclick="setSetTab('workflow')"><i class="ti ti-adjustments"></i> Workflow</button>
      <button class="settab-btn" data-t="git" onclick="setSetTab('git')"><i class="ti ti-git-branch"></i> Git</button>
      <button class="settab-btn" data-t="profile" onclick="setSetTab('profile')"><i class="ti ti-user-circle"></i> Profile</button>
      <button class="settab-btn" data-t="notify" onclick="setSetTab('notify')"><i class="ti ti-bell"></i> Notify</button>
      <button class="settab-btn" data-t="meetings" onclick="setSetTab('meetings')"><i class="ti ti-calendar-event"></i> Meetings</button>
      <button class="settab-btn" data-t="appear" onclick="setSetTab('appear')"><i class="ti ti-palette"></i> Appearance</button>
      <button class="settab-btn" data-t="integrations" onclick="setSetTab('integrations')"><i class="ti ti-plug-connected"></i> Integrations</button>
      <button class="settab-btn" data-t="mcp" onclick="setSetTab('mcp')"><i class="ti ti-api"></i> MCP</button>
    </div>
    <div class="settab" data-p="engines">
      ${detBanner}
      <div style="margin:-2px 0 12px"><button onclick="checkAgentSetup(true)" class="btn-ghost"><i class="ti ti-robot"></i> Agent setup guide</button></div>
      <div class="panel frm">${defRow}
        <div class="fr"><span class="lbl">Auto failover</span>
          <select id="eng-autofb"><option value="true" ${(cfg.engine&&cfg.engine.auto_fallback!==false)?'selected':''}>on</option><option value="false" ${(cfg.engine&&cfg.engine.auto_fallback===false)?'selected':''}>off</option></select>
          <span class="hint">on (default): auto-use every installed CLI + a cheaper tier as fallback — no manual list needed</span></div>
        <div class="fr" style="align-items:flex-start"><span class="lbl">Extra fallbacks</span>
          <textarea id="eng-fallbacks" rows="2" style="flex:1;min-width:0;background:var(--card);color:var(--text);border:1px solid var(--border2);border-radius:8px;padding:8px 11px;font-size:12.5px;font-family:ui-monospace,Menlo,monospace" placeholder="one per line: &lt;engine&gt; &lt;model&gt;\ne.g.  opencode gpt-4o\n      gemini gemini-2.0-flash">${(cfg.engine&&cfg.engine.fallbacks||[]).map(f=>`${f.engine} ${f.model}`).join("\n")}</textarea>
          <span class="hint">tried in order when the primary hits a quota/rate-limit wall or stalls (timeout). Same CLI, cheaper model works too, e.g. <code>claude haiku</code></span></div>
        <button type="button" class="set-expand ${anyOverride?'open':''}" onclick="toggleRoleOverrides(this)"><i class="ti ti-chevron-right"></i> Per-agent model overrides <span style="color:var(--dim);font-weight:400">· optional — give any agent a different model</span></button>
        <div class="role-overrides" ${anyOverride?'':'hidden'}>${roleRows}</div>
      </div>
    </div>
    <div class="settab" data-p="workflow" hidden>
      <div class="panel frm">
        <div class="fr"><span class="lbl">Mode</span><select id="wf-mode"><option value="kanban" ${wf.mode!=='scrum'?'selected':''}>kanban</option><option value="scrum" ${wf.mode==='scrum'?'selected':''}>scrum</option></select><span class="hint">scrum groups cycles into sprints</span></div>
        <div class="fr"><span class="lbl">Sprint length</span><input id="wf-sp" type="number" min="1" value="${wf.sprint_length_cycles??10}" style="width:90px"/><span class="hint">cycles per sprint</span></div>
        <div class="fr"><span class="lbl">BA every N cycles</span><input id="wf-ba" type="number" min="0" value="${wf.ba_every_n_cycles??4}" style="width:90px"/><span class="hint">0 disables BA</span></div>
        <div class="fr"><span class="lbl">Feature dev</span><select id="wf-fd"><option value="true" ${wf.feature_dev_enabled!==false?'selected':''}>enabled</option><option value="false" ${wf.feature_dev_enabled===false?'selected':''}>disabled</option></select></div>
        <div class="fr"><span class="lbl">Ops monitor</span><select id="wf-ops"><option value="true" ${wf.ops_monitor!==false?'selected':''}>on</option><option value="false" ${wf.ops_monitor===false?'selected':''}>off</option></select><span class="hint">pings the deployed app; files a bug + alerts on an outage</span></div>
        <div class="fr"><span class="lbl">Token saver</span><select id="wf-ts"><option value="true" ${wf.token_saver!==false?'selected':''}>on — compress diffs/logs &amp; terse agent output</option><option value="false" ${wf.token_saver===false?'selected':''}>off — full verbosity</option></select><span class="hint">cuts engine spend on big reviews with no loss of the actual change</span></div>
        <div class="fr"><span class="lbl">Agent sandbox</span><select id="wf-sbx"><option value="false" ${wf.sandbox!==true?'selected':''}>off — agents write anywhere your user can</option><option value="true" ${wf.sandbox===true?'selected':''}>on — file writes confined to this workspace (macOS)</option></select><span class="hint">a confused agent can't damage files outside the project; toolchain caches stay writable</span></div>
        <div class="fr"><span class="lbl">TDD gate</span><select id="wf-tdd"><option value="true" ${wf.tdd!==false?'selected':''}>on — TEST writes failing tests from acceptance criteria before DEV codes</option><option value="false" ${wf.tdd===false?'selected':''}>off</option></select><span class="hint">"done" becomes machine-checkable before implementation starts</span></div>
        <div class="fr"><span class="lbl">Cost approval gate</span><input id="wf-gate" type="number" step="0.5" min="0" placeholder="off" value="${wf.approve_over_usd??''}" style="width:110px"><span class="hint">USD — tickets estimated above this wait for your approval; empty = off</span></div>
        <div class="fr"><span class="lbl">Escalation ladder</span><input id="en-esc" placeholder="engine defaults (claude → opus; opencode → custom providers first)" value="${esc(((cfg.engine||{}).escalation||[]).join(', '))}" style="min-width:280px"><span class="hint">comma-separated models tried on RETRIES of a failed ticket, strongest last</span></div>
        <div class="fr"><span class="lbl">Scrum language</span><select id="wf-lang"><option value="en" ${wf.language!=='vi'?'selected':''}>English</option><option value="vi" ${wf.language==='vi'?'selected':''}>Tiếng Việt</option></select><span class="hint">standup, planning, grooming &amp; retro speak this language</span></div>
        <div class="fr"><span class="lbl">Sleep seconds</span><input id="wf-sl" type="number" min="0" value="${wf.sleep_seconds??30}" style="width:90px"/><span class="hint">between cycles</span></div>
        <div class="fr"><span class="lbl">Concurrency</span><input id="wf-cc" type="number" min="1" max="16" value="${wf.concurrency??1}" style="width:90px"/><span class="hint">parallel workers per role (dev/test/docs) — 1 = serial; higher runs several tickets of a role at once via per-ticket leases</span></div>
        <div class="fr"><span class="lbl">Budget — total (USD)</span><input id="wf-bg" type="number" min="0" step="0.5" value="${wf.budget_usd??''}" placeholder="unlimited" style="width:100px" ${wf.budget_usd==null?'disabled':''}/>
          <label class="hint" style="display:inline-flex;align-items:center;gap:5px;cursor:pointer"><input type="checkbox" id="wf-bg-unl" ${wf.budget_usd==null?'checked':''} onchange="document.getElementById('wf-bg').disabled=this.checked;if(this.checked)document.getElementById('wf-bg').value='';"> unlimited</label>
          <span class="hint">lifetime cap · pauses loop · <b style="color:var(--accent2)">applies live</b></span></div>
        <div class="fr"><span class="lbl">Budget — per day (USD)</span><input id="wf-dg" type="number" min="0" step="0.5" value="${pol.daily_budget_usd??''}" placeholder="unlimited" style="width:100px" ${pol.daily_budget_usd==null?'disabled':''}/>
          <label class="hint" style="display:inline-flex;align-items:center;gap:5px;cursor:pointer"><input type="checkbox" id="wf-dg-unl" ${pol.daily_budget_usd==null?'checked':''} onchange="document.getElementById('wf-dg').disabled=this.checked;if(this.checked)document.getElementById('wf-dg').value='';"> unlimited</label>
          <span class="hint">resets at UTC midnight · <b style="color:var(--accent2)">applies live</b></span></div>
        <div class="fr"><span class="lbl">Budget warn at (%)</span><input id="wf-bwp" type="number" min="1" max="100" step="5" value="${Math.round((pol.budget_warn_pct??0.8)*100)}" style="width:90px"/>
          <span class="hint">posts a one-time ⚠️ heads-up in #agents at this % of whichever cap is closer — loop keeps running</span></div>
      </div>
      <div class="set-note">Budget caps apply <b style="color:var(--accent2)">immediately</b>; other settings on the next restart. The total cap is <b>cumulative</b> — to resume past a hit cap, set it above what's already spent (or tick unlimited).</div>

      <div class="sec" style="margin-top:18px">Approval</div>
      <div class="panel frm">
        <div class="fr"><span class="lbl">Ready gate</span><select id="hu-ready"><option value="true" ${hu.gate_ready!==false?'selected':''}>on — a person approves each designed ticket</option><option value="false" ${hu.gate_ready===false?'selected':''}>off — designed tickets go straight to the queue</option></select><span class="hint">off = full autonomy: no one approves, work flows on its own</span></div>
        <div class="fr"><span class="lbl">Verify gate</span><select id="hu-verify"><option value="true" ${hu.gate_verify!==false?'selected':''}>on — a person renders the QA verdict on a fix</option><option value="false" ${hu.gate_verify===false?'selected':''}>off — the agent's tests are the verdict</option></select><span class="hint">verify is always a human when on — there is no auto-verify</span></div>
        <div class="fr"><span class="lbl">Auto-approve</span><select id="hu-adaptive"><option value="true" ${ad.enabled!==false?'selected':''}>on — routine tickets auto-approve, announced with an undo</option><option value="false" ${ad.enabled===false?'selected':''}>off — every ticket waits for a person</option></select><span class="hint">only when the Ready gate is on. The machine approves what it has learned is routine; you keep an undo window. Verify still needs you.</span></div>
        <div class="fr"><span class="lbl">Undo window</span><input id="hu-undo" type="number" min="0" value="${ad.undo_window_minutes??30}" style="width:90px"/><span class="hint">minutes to pull back an auto-approval · 0 disables auto-approve entirely</span></div>
        <div class="fr"><span class="lbl">Learn after</span><input id="hu-learn" type="number" min="1" value="${ad.learn_after_samples??8}" style="width:90px"/><span class="hint">consistent human decisions on one ticket shape before it shifts to auto</span></div>
        <div class="fr"><span class="lbl">Max auto / cycle</span><input id="hu-maxauto" type="number" min="1" value="${ad.max_auto_per_cycle??3}" style="width:90px"/><span class="hint">blast-radius cap — at most this many auto-approvals per cycle</span></div>
        <div class="fr"><span class="lbl">Route exceptions to</span><input id="hu-route" value="${esc(hu.route_exceptions_to||'')}" placeholder="username" style="width:160px"/><span class="hint">who a parked/exception ticket lands on</span></div>
        <div class="fr"><span class="lbl">Question SLA</span><input id="hu-sla" type="number" min="0" value="${hu.question_sla_minutes??60}" style="width:90px"/><span class="hint">minutes before an unanswered agent question escalates</span></div>
      </div>
      <div class="set-note">The three dials, weakest to strongest autonomy: <b>Ready gate off</b> (no approval at all) → <b>Auto-approve on</b> (routine auto, exceptions asked) → <b>Auto-approve off</b> (every ticket asked). Applies on the next restart.</div>
    </div>
    <div class="settab" data-p="git" hidden>
      ${toolingHtml}
      <div class="panel frm">
        <div class="fr"><span class="lbl">Integration</span><select id="git-en"><option value="false" ${!git.enabled?'selected':''}>disabled</option><option value="true" ${git.enabled?'selected':''}>enabled</option></select><span class="hint">off = agents never touch git; safe until you turn it on</span></div>
        <div class="fr"><span class="lbl">Provider</span><select id="git-pv"><option value="github" ${git.provider!=='gitlab'?'selected':''}>GitHub</option><option value="gitlab" ${git.provider==='gitlab'?'selected':''}>GitLab</option></select><span class="hint">GitHub via <code>gh</code> · GitLab via <code>glab</code></span></div>
        <div class="fr"><span class="lbl">Repository</span><input id="git-repo" value="${esc(git.repo||'')}" placeholder="owner/name — e.g. stevejrrogers/CoXChat" style="width:280px"/></div>
        <div class="fr"><span class="lbl">Base URL</span><input id="git-url" value="${esc(git.base_url||'')}" placeholder="blank = provider default (self-hosted only)" style="width:280px"/></div>
        <div class="fr"><span class="lbl">Default branch</span><input id="git-br" value="${esc(git.default_branch||'main')}" style="width:120px"/><span class="hint">the repo's main branch</span></div>
        <div class="fr"><span class="lbl">Target branch</span><input id="git-tb" value="${esc(git.target_branch||'')}" placeholder="blank = default" style="width:140px"/><span class="hint">agent PRs open into &amp; auto-merge here (e.g. <code>develop</code>)</span></div>
        <div class="fr"><span class="lbl">Branch prefix</span><input id="git-bp" value="${esc(git.branch_prefix||'feat/')}" style="width:120px"/><span class="hint">→ ${esc(git.branch_prefix||'feat/')}CXC-123</span></div>
        <div class="fr"><span class="lbl">Commit email</span><input id="git-em" value="${esc(git.commit_email||'')}" placeholder="…@users.noreply.github.com" style="width:280px"/><span class="hint">use a noreply email to avoid privacy blocks</span></div>
        <div class="fr"><span class="lbl">Act as account</span>${gitAccountControl(ga, git.account||'')}<span class="hint" id="git-acct-hint">sign in twice (<code>gh auth login</code>) and pick the one this project uses — two projects can then be two different users at once</span></div>
        <div class="fr"><span class="lbl">Open PR/MR</span><select id="git-pr"><option value="true" ${git.auto_pr!==false?'selected':''}>automatically after push</option><option value="false" ${git.auto_pr===false?'selected':''}>manual</option></select></div>
        <div class="fr"><span class="lbl">Auto-review</span><select id="git-ar"><option value="true" ${git.auto_review!==false?'selected':''}>on — the SA agent reviews every PR &amp; suggests</option><option value="false" ${git.auto_review===false?'selected':''}>off — no automatic review</option></select><span class="hint">SA deep-dives each PR and posts approve / request-changes as a suggestion</span></div>
        <div class="fr"><span class="lbl">Auto-merge</span><select id="git-am"><option value="false" ${!git.auto_merge?'selected':''}>off — you merge from the Review tab</option><option value="true" ${git.auto_merge?'selected':''}>on — SA approves &amp; merges automatically</option></select><span class="hint">On: SA merges on approve (never on failing CI). Off: approval is only a suggestion; request-changes still loops back to the agent to fix.</span></div>
      </div>
    </div>
    <div class="settab" data-p="mcp" hidden><div id="mcp-panel"></div></div>
    <div class="settab" data-p="profile" hidden>
      <div class="panel frm">
        <div class="fr"><span class="lbl">Username</span><span style="padding:9px 0;font-weight:600">${esc(ME?.username||'')}</span></div>
        <div class="fr"><span class="lbl">Name</span><input id="prof-name" value="${esc(ME?.name||'')}" placeholder="Your display name" style="flex:1;max-width:280px"/></div>
        <div class="fr"><span class="lbl">Email</span><input id="prof-email" value="${esc(ME?.email||'')}" placeholder="your@email.com" style="flex:1;max-width:280px"/></div>
        <div class="fr"><span class="lbl">New password</span><input id="prof-pw" type="password" placeholder="leave blank to keep" style="flex:1;max-width:220px"/></div>
        <button class="gc-btn pri" onclick="saveProfile()" style="margin-top:8px"><i class="ti ti-device-floppy"></i> Update profile</button>
        <div id="prof-note" style="margin-top:6px;font-size:12px"></div>
      </div>
    </div>
    <div class="settab" data-p="notify" hidden>
      <div class="panel frm">
        <div class="fr"><span class="lbl">Desktop notifications</span><select id="ntf-desktop"><option value="true" selected>on</option><option value="false">off</option></select></div>
        <div class="fr"><span class="lbl">Sound</span><select id="ntf-sound"><option value="true" selected>on</option><option value="false">off</option></select></div>
        <div class="fr"><span class="lbl">Meeting reminder</span><input id="ntf-meet" type="number" min="0" max="60" value="${localStorage.getItem('cox_meet_remind')||5}" style="width:70px"/><span class="hint">minutes before</span></div>
        <button class="gc-btn pri" onclick="saveNotify()" style="margin-top:8px"><i class="ti ti-device-floppy"></i> Save</button>
      </div>
    </div>
    <div class="settab" data-p="meetings" hidden>
      <div class="panel frm">
        <div class="fr"><span class="lbl">Default duration</span><input id="mtg-dur" type="number" min="5" max="480" value="${localStorage.getItem('cox_mtg_dur')||30}" style="width:80px"/><span class="hint">minutes</span></div>
        <div class="fr"><span class="lbl">Default reminder</span><input id="mtg-remind" type="number" min="0" max="60" value="${localStorage.getItem('cox_mtg_remind')||5}" style="width:80px"/><span class="hint">minutes before</span></div>
        <div class="fr"><span class="lbl">Auto-ring</span><select id="mtg-ring"><option value="true">yes — ring participants at start</option><option value="false">no</option></select></div>
        <button class="gc-btn pri" onclick="saveMeetings()" style="margin-top:8px"><i class="ti ti-device-floppy"></i> Save</button>
      </div>
    </div>
    <div class="settab" data-p="appear" hidden>
      <div class="panel frm">
        <div class="fr"><span class="lbl">Theme</span><select id="app-theme" onchange="applyTheme(this.value)"><option value="dark">Dark</option><option value="light">Light</option></select></div>
        <div class="fr"><span class="lbl">Accent color</span><input id="app-accent" type="color" value="${localStorage.getItem('cox_accent')||'#38bdf8'}" onchange="applyAccent(this.value)" style="width:50px;height:34px;border:none;cursor:pointer"/></div>
        <div class="fr"><span class="lbl">Font size</span><select id="app-fs" onchange="applyFontSize(this.value)"><option value="13">Small</option><option value="14" selected>Medium</option><option value="16">Large</option></select></div>
      </div>
    </div>
    <div class="settab" data-p="integrations" hidden>
      <div class="panel frm">
        <div class="fr" style="align-items:flex-start"><span class="lbl">Webhooks</span><div style="flex:1;font-size:12px;color:var(--muted)">Let external tools post into channels. Manage webhooks from each channel's <b># channel header → Webhook button</b>.</div></div>
        <button class="gc-btn" onclick="openWebhooks()" style="margin-top:6px"><i class="ti ti-webhook"></i> Manage webhooks</button>
      </div>
    </div>
    <div class="set-footer"><button class="save" onclick="saveSettings()"><i class="ti ti-device-floppy"></i> Save changes</button><span id="save-note"></span><span class="set-foothint">Engine &amp; model changes apply on the next cycle — no restart</span></div>`;
  setSetTab(window._setTab==="workspace"?"engines":(window._setTab||"engines"));}
function copyText(btn,text){navigator.clipboard&&navigator.clipboard.writeText(text);
  const old=btn.innerHTML;btn.innerHTML='<i class="ti ti-check"></i>';setTimeout(()=>{btn.innerHTML=old;},1200);}
// "Act as account" — a <select> of detected `gh auth status` accounts when
// the CLI is present and signed in (with a "Default = active login" option),
// falling back to a free-text input when nothing was detected. A configured
// account that isn't in the detected list stays as an extra option so saving
// never silently drops it.
function gitAccountControl(ga, current){
  const accts = (ga && Array.isArray(ga.accounts)) ? ga.accounts : [];
  if(!ga || !ga.present || accts.length === 0){
    return `<input id="git-acct" value="${esc(current||'')}" placeholder="blank = the CLI's active login" style="width:280px"/>`;
  }
  const cur = current||'';
  const names = accts.map(a => a.name);
  const hasCur = !cur || names.includes(cur);
  const opts = [`<option value="" ${cur===''?'selected':''}>Default — the CLI's active login</option>`];
  accts.forEach(a => {
    const tag = a.active ? ' (active)' : '';
    opts.push(`<option value="${esc(a.name)}" ${cur===a.name?'selected':''}>${esc(a.name)}${tag}</option>`);
  });
  if(!hasCur){
    opts.push(`<option value="${esc(cur)}" selected>${esc(cur)} (not detected)</option>`);
  }
  return `<select id="git-acct" style="max-width:280px">${opts.join('')}</select>`;
}
async function testGitConnection(btn){
  const o=btn.innerHTML;btn.innerHTML='<i class="ti ti-loader-2 att-spin"></i> Testing…';btn.disabled=true;
  const box=document.getElementById("git-test-result");
  try{
    const d=await(await fetch(api("/git/test"),{method:"POST"})).json();
    const step=(ok,label,extra)=>`<span style="display:inline-flex;align-items:center;gap:5px;margin-right:14px"><i class="ti ti-${ok?'circle-check':'circle-x'}" style="color:${ok?'var(--green)':'var(--red)'}"></i>${label}${extra||''}</span>`;
    box.innerHTML=`<div style="padding:9px 12px;font-size:12.5px;border:1px solid var(--border);border-radius:9px;margin-top:8px">
      ${step(d.repo,'git repo')}
      ${step(!!d.remote,'remote',d.remote?` <code style="font-size:11px">${esc(d.remote)}</code>`:' — none: agents cannot push; add one (git remote add origin …) or reconnect')}
      ${step(d.reachable,'reachable')}
      ${step(d.push_ok,'push permission')}
      ${step(d.api_ok,'pull requests',d.api_account?` <code style="font-size:11px">${esc(d.api_account)}</code>`:'')}
      ${d.probed_on?`<div style="color:var(--dim);margin-top:5px">checked on <code style="font-size:11px">${esc(d.probed_on)}</code> — the machine that runs the agents</div>`:''}
      ${d.detail?`<div style="color:var(--dim);margin-top:5px">${esc(d.detail)}</div>`:''}
      ${d.key_hint?`<div style="color:var(--amber);margin-top:5px"><i class="ti ti-key"></i> ${esc(d.key_hint)}</div>`:''}
      ${d.api_detail?`<div style="color:var(--amber);margin-top:5px"><i class="ti ti-alert-triangle"></i> ${esc(d.api_detail)}</div>`:''}
    </div>`;
  }catch(e){box.innerHTML='<div class="empty">test failed</div>';}
  btn.innerHTML=o;btn.disabled=false;
}
function openConnect(provider){const gl=provider==="gitlab";
  const base=(window._cfg&&window._cfg.git&&window._cfg.git.base_url||"").replace(/\/+$/,"");
  const host=base||(gl?"https://gitlab.com":"https://github.com");
  document.getElementById("cn-prov").textContent=gl?"GitLab":"GitHub";
  document.getElementById("cn-prov2").textContent=gl?"GitLab":"GitHub";
  document.getElementById("cn-cli").textContent=gl?"glab":"gh";
  document.getElementById("cn-scope").textContent=gl?"scopes: api, write_repository":"scopes: repo, read:org";
  const link=gl?host+"/-/user_settings/personal_access_tokens?name=CoXAgent"
              :host+"/settings/tokens/new?scopes=repo,read:org&description=CoXAgent";
  document.getElementById("cn-link").href=link;
  document.getElementById("cn-token").value="";document.getElementById("cn-msg").textContent="";
  document.getElementById("ov-connect").classList.add("open");
  setTimeout(()=>document.getElementById("cn-token").focus(),40);}
async function submitConnect(){const token=document.getElementById("cn-token").value.trim();const msg=document.getElementById("cn-msg");
  if(!token){msg.style.color="var(--red)";msg.textContent="Paste a token first.";return;}
  msg.style.color="var(--muted)";msg.textContent="Connecting…";
  try{const r=await fetch(api("/git/connect"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({token})});
    if(r.ok){const d=await r.json();close_("ov-connect");toasty("Connected"+(d.account?" as "+d.account:""),"ok");loadSettings();}
    else{msg.style.color="var(--red)";msg.textContent=(await r.text())||"Sign-in failed.";}
  }catch(e){msg.style.color="var(--red)";msg.textContent="Network error.";}}
function setSetTab(name){window._setTab=name;
  document.querySelectorAll(".settab-btn").forEach(b=>b.classList.toggle("on",b.dataset.t===name));
  document.querySelectorAll(".settab").forEach(p=>{p.hidden=p.dataset.p!==name;});
  if(name==="mcp")renderMcpPanel();
}
