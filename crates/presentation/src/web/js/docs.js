// Wiki editor: rich text, live collaboration, HTML→Markdown.
// Split from index.html — classic script, load order matters (one shared scope).
// ---- Rich-text (Confluence-like) editor + live collaboration --------------
let DOCWS=null, DOCWS_ID=null, RT_CONN=Math.random().toString(36).slice(2),
    RT_TYPING=0, RT_SAVE_T=null, RT_PING_T=null, RT_APPLYING=false;
function rtRich(){return document.getElementById("doc-rich");}
function rtFocusEdit(){const r=rtRich();if(r){r.focus();}}
function rtCmd(cmd,val){document.execCommand(cmd,false,val||null);rtFocusEdit();rtDirty();}
function rtBlock(tag){document.execCommand("formatBlock",false,tag);rtFocusEdit();rtDirty();}
function rtInsert(html){document.execCommand("insertHTML",false,html);rtDirty();}
function rtInlineCode(){const s=window.getSelection();const t=s&&s.toString();if(t)rtInsert('<code>'+esc(t)+'</code>&nbsp;');else rtInsert('<code>code</code>&nbsp;');}
function rtCodeBlock(){rtInsert('<pre class="codeblock">code</pre><p><br></p>');}
function rtChecklist(){rtInsert('<ul class="tasklist"><li data-checked="false">To do</li></ul><p><br></p>');}
function rtHr(){rtInsert('<hr><p><br></p>');}
function rtTable(){let h='<table><tbody>';for(let r=0;r<3;r++){h+='<tr>';for(let c=0;c<3;c++){h+=(r===0?'<th>Head '+(c+1)+'</th>':'<td>&nbsp;</td>');}h+='</tr>';}h+='</tbody></table><p><br></p>';rtInsert(h);}
async function rtLink(){const s0=window.getSelection();const saved=(s0&&s0.rangeCount)?s0.getRangeAt(0).cloneRange():null;
  const url=await coxModal({title:"Insert link",message:"URL để chèn vào trang.",input:{placeholder:"https://…",value:"https://"},confirmText:"Insert"});if(!url)return;
  if(saved){const s=window.getSelection();s.removeAllRanges();s.addRange(saved);}
  const s=window.getSelection();const t=(s&&s.toString())||url;if(s&&s.toString())document.execCommand("createLink",false,url);else rtInsert('<a href="'+esc(url)+'" target="_blank" rel="noopener">'+esc(t)+'</a>&nbsp;');rtDirty();}
function docEditInit(d){
  const rich=rtRich();if(!rich)return;
  // Enter makes <p> (not <div>) so the HTML→Markdown pass is predictable.
  try{document.execCommand("defaultParagraphSeparator",false,"p");}catch(e){}
  rich.addEventListener("input",rtDirty);
  // Toggle a task-list checkbox by clicking its box (left gutter).
  rich.addEventListener("click",ev=>{
    const li=ev.target.closest&&ev.target.closest("li");
    if(li&&li.parentElement&&li.parentElement.classList.contains("tasklist")&&ev.offsetX<22){
      li.setAttribute("data-checked",li.getAttribute("data-checked")==="true"?"false":"true");rtDirty();
    }
  });
  docsWsConnect(d.id);
  setTimeout(rtFocusEdit,30);
}
function rtDirty(){
  if(RT_APPLYING)return;
  RT_TYPING=Date.now();
  const st=document.getElementById("rt-status");if(st)st.textContent="Editing…";
  clearTimeout(RT_SAVE_T);RT_SAVE_T=setTimeout(rtAutosave,600);
}
function rtCollect(){
  const d=currentDoc();
  const title=(document.getElementById("doc-title")||{}).value||"";
  const folder=(document.getElementById("doc-folder")||{}).value||"";
  const body=htmlToMd(rtRich());
  return {id:(d&&d.id)||DOCWS_ID,title:title.trim()||(d&&d.title)||"Untitled",folder:folder.trim(),body};
}
function rtAutosave(){
  const p=rtCollect();if(!p.id)return;
  const st=document.getElementById("rt-status");
  // Prefer the live socket (persists + fans out); fall back to REST.
  if(DOCWS&&DOCWS.readyState===1){
    DOCWS.send(JSON.stringify({op:"save",origin:RT_CONN,title:p.title,folder:p.folder,body:p.body}));
    if(st)st.innerHTML='<i class="ti ti-cloud-check"></i> Saved';
  }else{
    putDoc(p.id,p.folder,p.title,p.body).then(()=>{if(st)st.textContent="Saved (offline)";});
  }
  // Keep the local model fresh so the reader view matches on exit.
  const d=currentDoc();if(d){d.title=p.title;d.folder=p.folder;d.body=p.body;}
}
async function saveDoc(){
  clearTimeout(RT_SAVE_T);
  const p=rtCollect();
  if(!(DOCWS&&DOCWS.readyState===1)){await putDoc(p.id,p.folder,p.title,p.body);}
  else{rtAutosave();}
  docsWsClose();DOC_EDIT=false;
  try{DOCS=await(await fetch(api("/docs"))).json();}catch(e){}
  renderDocsList();renderDocMain();
}
function cancelEdit(){docsWsClose();DOC_EDIT=false;loadDocs();}
// ---- Live collaboration socket --------------------------------------------
function docsWsClose(){if(RT_PING_T){clearInterval(RT_PING_T);RT_PING_T=null;}if(DOCWS){try{DOCWS.close();}catch(e){}DOCWS=null;}DOCWS_ID=null;}
function docsWsConnect(id){
  docsWsClose();DOCWS_ID=id;
  try{
    const proto=location.protocol==="https:"?"wss":"ws";
    DOCWS=new WebSocket(proto+"://"+location.host+api("/docs/"+encodeURIComponent(id)+"/ws"));
    DOCWS.onmessage=e=>{try{onDocWs(JSON.parse(e.data));}catch(_){}};
    DOCWS.onopen=()=>{RT_PING_T=setInterval(()=>{if(DOCWS&&DOCWS.readyState===1)DOCWS.send(JSON.stringify({op:"ping"}));},15000);};
  }catch(e){}
}
function onDocWs(m){
  if(m.op==="presence"){renderPresence(m.editors||[]);return;}
  if(m.op==="doc"){
    if(m.id!==DOC_CUR)return;
    if(m.origin===RT_CONN)return; // our own echo
    // Update local model + list from a teammate's edit.
    const d=currentDoc();if(d){d.title=m.title;d.folder=m.folder;d.body=m.body;}
    renderDocsList();
    if(!DOC_EDIT){renderDocMain();return;}
    // While editing: apply the incoming content only when we're idle, so we
    // never yank the caret mid-keystroke (last-writer, non-destructive).
    if(Date.now()-RT_TYPING>1400){
      RT_APPLYING=true;
      const rich=rtRich();if(rich)rich.innerHTML=mdRender(m.body)||"<p><br></p>";
      const tt=document.getElementById("doc-title");if(tt&&document.activeElement!==tt)tt.value=m.title;
      RT_APPLYING=false;
      const st=document.getElementById("rt-status");if(st)st.innerHTML='<i class="ti ti-users"></i> Updated by '+esc(m.by);
    }else{
      const st=document.getElementById("rt-status");if(st)st.innerHTML='<i class="ti ti-users"></i> '+esc(m.by)+' is editing…';
    }
  }
}
function renderPresence(editors){
  const box=document.getElementById("rt-people");if(!box)return;
  const n=editors.length; // unique usernames currently editing this page
  const avs=editors.slice(0,5).map(u=>`<span class="rt-av" title="${esc(u)}">${esc((u[0]||"?").toUpperCase())}</span>`).join("");
  const label=n>1?`<span class="rt-live"><i class="ti ti-point-filled"></i>${n} editing live</span>`:`<span class="rt-live"><i class="ti ti-point-filled"></i>Live</span>`;
  box.innerHTML=`<div style="display:flex">${avs}</div>${label}`;
}
// ---- HTML (contenteditable DOM) → Markdown --------------------------------
function htmlToMd(root){
  if(!root)return"";
  const inline=node=>{
    let out="";
    node.childNodes.forEach(n=>{
      if(n.nodeType===3){out+=n.nodeValue;return;}
      if(n.nodeType!==1)return;
      const t=n.tagName.toLowerCase(),inner=inline(n);
      if(t==="br")out+="\n";
      else if(t==="b"||t==="strong")out+=inner.trim()?"**"+inner+"**":"";
      else if(t==="i"||t==="em")out+=inner.trim()?"*"+inner+"*":"";
      else if(t==="del"||t==="s"||t==="strike")out+=inner.trim()?"~~"+inner+"~~":"";
      else if(t==="code")out+="`"+n.textContent+"`";
      else if(t==="a")out+="["+inner+"]("+(n.getAttribute("href")||"")+")";
      else out+=inner;
    });
    return out;
  };
  const cellText=n=>inline(n).replace(/\n/g," ").replace(/\|/g,"\\|").trim();
  let md="";
  const block=el=>{
    el.childNodes.forEach(n=>{
      if(n.nodeType===3){const tx=n.nodeValue.trim();if(tx)md+=tx+"\n\n";return;}
      if(n.nodeType!==1)return;
      const t=n.tagName.toLowerCase();
      if(/^h[1-6]$/.test(t)){md+="#".repeat(+t[1])+" "+inline(n).trim()+"\n\n";}
      else if(t==="p"||t==="div"){const s=inline(n).trim();md+=s?s+"\n\n":"";}
      else if(t==="br"){md+="\n";}
      else if(t==="ul"&&n.classList.contains("tasklist")){n.querySelectorAll(":scope>li").forEach(li=>{md+="- ["+(li.getAttribute("data-checked")==="true"?"x":" ")+"] "+inline(li).trim()+"\n";});md+="\n";}
      else if(t==="ul"){n.querySelectorAll(":scope>li").forEach(li=>{md+="- "+inline(li).trim()+"\n";});md+="\n";}
      else if(t==="ol"){let i=1;n.querySelectorAll(":scope>li").forEach(li=>{md+=(i++)+". "+inline(li).trim()+"\n";});md+="\n";}
      else if(t==="blockquote"){inline(n).trim().split("\n").forEach(l=>{md+="> "+l+"\n";});md+="\n";}
      else if(t==="pre"){md+="```\n"+n.textContent.replace(/\n$/,"")+"\n```\n\n";}
      else if(t==="hr"){md+="---\n\n";}
      else if(t==="table"){
        const rows=[...n.querySelectorAll("tr")];if(!rows.length)return;
        const cells=r=>[...r.children].map(cellText);
        const head=cells(rows[0]);
        md+="| "+head.join(" | ")+" |\n| "+head.map(()=>"---").join(" | ")+" |\n";
        rows.slice(1).forEach(r=>{md+="| "+cells(r).join(" | ")+" |\n";});
        md+="\n";
      }
      else{const s=inline(n).trim();md+=s?s+"\n\n":"";}
    });
  };
  block(root);
  return md.replace(/\n{3,}/g,"\n\n").trim()+"\n";
}
async function putDoc(id, folder, title, body){
  try{
    const r=await fetch(api("/docs/"+encodeURIComponent(id||("new-"+Date.now()))),{method:"PUT",headers:{"Content-Type":"application/json"},body:JSON.stringify({folder,title,body})});
    if(!r.ok){toasty(await r.text()||"save failed","err");return;}
    const page=await r.json();DOC_CUR=page.id;
    try{DOCS=await(await fetch(api("/docs"))).json();}catch(e){}
    renderDocsList();
  }catch(e){toasty("save failed","err");}
}
async function askAiEdit(){
  const d=currentDoc();if(!d)return;
  const instruction=await coxModal({title:"AI revise",message:"AI nên sửa trang này thế nào?",input:{placeholder:"e.g. add a section on error handling; make it shorter and clearer",multiline:true},confirmText:"Revise"});
  if(!instruction||!instruction.trim())return;
  const el=document.getElementById("docs-main");
  el.innerHTML='<div class="docs-empty"><i class="ti ti-loader-2 att-spin"></i><div>DOCS agent is revising…</div></div>';
  try{
    const r=await fetch(api("/docs/"+encodeURIComponent(d.id)+"/ai-edit"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({instruction:instruction.trim()})});
    if(!r.ok){toasty("AI edit failed: "+(await r.text()||r.status)+" (needs a configured engine).","err");}
    else{const page=await r.json();DOC_CUR=page.id;try{DOCS=await(await fetch(api("/docs"))).json();}catch(e){}toasty("Page revised by DOCS agent","ok");}
  }catch(e){toasty("AI edit failed","err");}
  renderDocsList();renderDocMain();
}
async function deleteDoc(){
  const d=currentDoc();if(!d)return;
  if(!await coxModal({title:"Delete page",message:'Xoá trang "'+(d.title||d.id)+'"? Không hoàn tác được.',danger:true,confirmText:"Delete"}))return;
  try{await fetch(api("/docs/"+encodeURIComponent(d.id)),{method:"DELETE"});}catch(e){}
  DOC_CUR=null;DOC_EDIT=false;await loadDocs();
}
async function generateDocs(){
  const btn=document.getElementById("docs-gen");const old=btn.innerHTML;btn.innerHTML='<i class="ti ti-loader-2 att-spin"></i>';btn.disabled=true;
  // Generation runs DOCS/SA/TEST agents — it can take 1–3 minutes. Show a
  // clear in-pane progress state (with an elapsed timer) so it never looks stuck.
  const main=document.getElementById("docs-main");
  const t0=Date.now();
  const paint=()=>{if(main)main.innerHTML=`<div class="docs-empty"><i class="ti ti-loader-2 att-spin" style="font-size:30px"></i><div>DOCS, SA &amp; TEST agents are writing…</div><span>Product, Technical, Flows &amp; Testing pages · ${Math.round((Date.now()-t0)/1000)}s elapsed — this can take a minute or two.</span></div>`;};
  paint();const tick=setInterval(paint,1000);
  try{
    const r=await fetch(api("/docs/generate"),{method:"POST"});
    if(!r.ok){toasty("Docs generation failed: "+(await r.text()||r.status)+" (needs a configured engine).","err");}
    else{const j=await r.json();toasty("Generated "+(j.pages||0)+" pages","ok");await loadDocs();}
  }catch(e){toasty("Docs generation failed","err");}
  clearInterval(tick);btn.innerHTML=old;btn.disabled=false;renderDocMain();
}
// Minimal Markdown → HTML for doc pages (headings, lists, code, links).
function mdRender(md){
  if(!md)return"";
  const blocks=[];
  let s=md.replace(/```([\s\S]*?)```/g,(_,c)=>{blocks.push('<pre class="codeblock">'+esc(c.replace(/^\n/,"").replace(/\n$/,""))+'</pre>');return "\nZZCODEBLK"+(blocks.length-1)+"ZZ\n";});
  const inline=t=>esc(t)
    .replace(/`([^`]+)`/g,'<code class="inlinecode">$1</code>')
    .replace(/\*\*([^*]+)\*\*/g,"<b>$1</b>").replace(/(^|[^*])\*([^*\n]+)\*/g,"$1<i>$2</i>")
    .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g,'<a href="$2" target="_blank" rel="noopener">$1</a>')
    .replace(/(^|[^"'>])(https?:\/\/[^\s<]+)/g,'$1<a href="$2" target="_blank" rel="noopener">$2</a>');
  const lines=s.split(/\r?\n/);let out="",inUl=false,inOl=false,inTask=false;
  const closeLists=()=>{if(inUl){out+="</ul>";inUl=false;}if(inOl){out+="</ol>";inOl=false;}if(inTask){out+="</ul>";inTask=false;}};
  const cells=r=>r.trim().replace(/^\|/,"").replace(/\|$/,"").split("|").map(c=>c.trim());
  for(let i=0;i<lines.length;i++){
    const ln=lines[i];
    const ph=ln.match(/^ZZCODEBLK(\d+)ZZ$/);if(ph){closeLists();out+=blocks[+ph[1]];continue;}
    // Markdown table: a "| … |" row followed by a "| --- |" separator.
    if(/^\s*\|.*\|\s*$/.test(ln)&&i+1<lines.length&&/^\s*\|?[\s:|-]+\|?\s*$/.test(lines[i+1])&&/-/.test(lines[i+1])){
      closeLists();let j=i;const rows=[];
      while(j<lines.length&&/^\s*\|.*\|\s*$/.test(lines[j])){rows.push(lines[j]);j++;}
      out+="<table><thead><tr>"+cells(rows[0]).map(c=>"<th>"+inline(c)+"</th>").join("")+"</tr></thead><tbody>";
      for(let k=2;k<rows.length;k++){out+="<tr>"+cells(rows[k]).map(c=>"<td>"+inline(c)+"</td>").join("")+"</tr>";}
      out+="</tbody></table>";i=j-1;continue;
    }
    let m;
    if(m=ln.match(/^(#{1,4})\s+(.*)$/)){closeLists();const n=m[1].length;out+="<h"+n+">"+inline(m[2])+"</h"+n+">";continue;}
    if(m=ln.match(/^\s*[-*]\s+\[([ xX])\]\s+(.*)$/)){if(!inTask){closeLists();out+='<ul class="tasklist">';inTask=true;}out+='<li data-checked="'+(m[1].toLowerCase()==="x"?"true":"false")+'">'+inline(m[2])+"</li>";continue;}
    if(/^\s*[-*]\s+/.test(ln)){if(!inUl){closeLists();out+="<ul>";inUl=true;}out+="<li>"+inline(ln.replace(/^\s*[-*]\s+/,""))+"</li>";continue;}
    if(/^\s*\d+\.\s+/.test(ln)){if(!inOl){closeLists();out+="<ol>";inOl=true;}out+="<li>"+inline(ln.replace(/^\s*\d+\.\s+/,""))+"</li>";continue;}
    if(/^\s*>\s?/.test(ln)){closeLists();out+="<blockquote>"+inline(ln.replace(/^\s*>\s?/,""))+"</blockquote>";continue;}
    if(/^\s*(---|\*\*\*|___)\s*$/.test(ln)){closeLists();out+="<hr>";continue;}
    if(!ln.trim()){closeLists();continue;}
    closeLists();out+="<p>"+inline(ln)+"</p>";
  }
  closeLists();return out;
}
function startPoll(){if(poll)return;poll=setInterval(async()=>{try{const s=await(await fetch(api("/state"))).json();const rn=await(await fetch(api("/runner"))).json();render(s);renderRunner(rn);setConn(false);}catch(e){}},3000);}
function connect(){if(ES)ES.close();if(poll){clearInterval(poll);poll=null;}
  ES=new EventSource(api("/events"));ES.onmessage=e=>{try{handle(JSON.parse(e.data));if(poll){clearInterval(poll);poll=null;}}catch(_){}};ES.onerror=()=>{setConn(false);startPoll();};}
function loadBudget(){fetch(api("/config")).then(r=>r.json()).then(c=>{window._budget=(c.workflow&&c.workflow.budget_usd)||null;
  // Cache the whole config: the agent cards read engine.per_role for the
  // engine badge before any run of a role has finished (see core.js).
  window._cfg=c;if(CUR==="insights"||CUR==="team")renderActive();}).catch(()=>{});}
let PROJECTS=[];
function projInitial(p){return (p.alias||p.name||"P").trim().charAt(0).toUpperCase()||"P";}
