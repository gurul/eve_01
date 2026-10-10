/* Local learning workspace: no remote scripts or browser API keys. This file has no build step; the whiteboard it mounts
   (tldraw, window.EveCanvas) is built from bridge/web-canvas into web/canvas. */
'use strict';
const $ = s => document.querySelector(s);
const escapeHTML = v => String(v ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
let config, lesson, dirty = false, busy = false, saveTimer, draftTimer, checkTimer;
let speak = false, supervise = false, tour = false, lastRevision = 0, saveInFlight = false;
/* Think out loud. listening mirrors GET /api/listening; ideasBase is the revision whose ideas text this page last loaded,
   so the server can put back a spoken line that arrived while the learner was typing. */
let listening = {available:false, state:'off', lesson_id:null, reason:''}, listenBusy = false, ideasBase = 0;
const main = $('#main');
function fail(error) { $('#error').textContent = error.message || error; $('#error').classList.remove('hidden'); }
function clearError() { $('#error').classList.add('hidden'); }
function say(text) { if (speak && 'speechSynthesis' in window) { speechSynthesis.cancel(); speechSynthesis.speak(new SpeechSynthesisUtterance(text)); } }
async function api(path, data) {
  const r = await fetch(path, data ? {method:'POST',headers:{'Content-Type':'application/json','X-Eve-Token':config.token},body:JSON.stringify(data)} : {});
  const result = await r.json(); if (!r.ok) throw new Error(result.error || 'Request failed'); return result;
}
function status(text) { $('#save-status').textContent = text; }
/* The Think out loud toggle stays usable while the tutor works: a learner must always be able to stop the microphone. */
function setBusy(value) { busy = value; document.querySelectorAll('main button:not(.tl-container *), main textarea:not(.tl-container *), main input:not(.tl-container *), #new-side, #dashboard-link').forEach(b => { if ('disabled' in b && b.id !== 'think-aloud') b.disabled=value; }); syncBoard(); }
/* Card art: a glyph for the topic. Math topics keep their expressions; any other subject gets the first letters of the topic on the plain sun background. */
const art = topic => /calculus|derivative/i.test(topic) ? ['calculus','dy / dx'] : /algebra|equation/i.test(topic) ? ['algebra','2x + 3 = 11'] : /\b(add|sum|fraction|arithmetic|count|multipl|divi|number)/i.test(topic) ? ['', '7 + 5 = ?'] : ['', escapeHTML(topic.trim().split(/\s+/).slice(0,2).map(w=>w[0]||'').join('').toUpperCase() || '?')];
const readableStage = stage => ({setup:'Ready to begin',input:'Add your problem',confirm:'Confirm problem',working:'In progress',complete:'Completed',ended:'Saved for later'}[stage] || stage);
/* Static robot drawings from the Show-and-Tell deck. SVG presentation attributes are CSP-safe; no animation. */
const ROBOT = `<svg viewBox="0 0 200 240" role="img" aria-label="eve, a small robot with a screen face"><defs><linearGradient id="bb-hero-label" x1="0" x2="1" y1="0" y2="1"><stop offset="0" stop-color="#9DB3EE"></stop><stop offset="1" stop-color="#B98BD8"></stop></linearGradient></defs><rect x="68" y="168" width="64" height="30" fill="#8C90A0" stroke="#4D5160" stroke-width="2.5"></rect><rect x="46" y="194" width="108" height="38" rx="7" fill="#6F7384" stroke="#4D5160" stroke-width="2.5"></rect><circle cx="64" cy="220" r="5" fill="#4D5160"></circle><circle cx="136" cy="220" r="5" fill="#4D5160"></circle><rect x="22" y="10" width="156" height="34" rx="7" fill="url(#bb-hero-label)" stroke="#5E5F98" stroke-width="2.5"></rect><text x="100" y="35" text-anchor="middle" font-family="Grandstander, sans-serif" font-weight="700" font-size="22" fill="#F4F4FB">eve</text><rect x="12" y="42" width="176" height="14" rx="4" fill="#A487D0" stroke="#5E5F98" stroke-width="2.5"></rect><rect x="30" y="52" width="140" height="122" rx="14" fill="#C3C6CF" stroke="#5C6070" stroke-width="3"></rect><rect x="42" y="62" width="116" height="100" rx="6" fill="#1B2350"></rect><path fill="#FF74D4" d="M72 75 H88 V80 H92 V94 H87 V89 H73 V94 H68 V80 H72 Z M112 75 H128 V80 H132 V94 H127 V89 H113 V94 H108 V80 H112 Z"></path><text x="100" y="134" text-anchor="middle" font-family="VT323, monospace" font-size="22" fill="#FF74D4">Hey there!</text></svg>`;
const FACE = `<svg viewBox="26 48 148 130" aria-hidden="true" focusable="false"><rect x="30" y="52" width="140" height="122" rx="14" fill="#C3C6CF" stroke="#5C6070" stroke-width="3"></rect><rect x="42" y="62" width="116" height="100" rx="6" fill="#1B2350"></rect><g fill="#FF74D4"><rect x="68" y="84" width="24" height="10" rx="3"></rect><rect x="108" y="84" width="24" height="10" rx="3"></rect></g></svg>`;
const FEED_LABEL = {hint:'a hint', check:'checking your work', step:'one step · eve’s work', recap:'recap', generate:'your problem', recognize:'reading your problem', confirm:'let’s go', end:'saved for later'};
async function dashboard() {
  if (busy) return;
  clearTimeout(checkTimer); await flush(); lesson=null; $('#breadcrumb').textContent='Learning dashboard';
  const rows=await api('/api/lessons');
  main.innerHTML=`<section class="hero"><div><span class="kicker">a little curiosity goes a long way</span><h1>Big ideas start<br>with little steps.</h1><p>Think out loud, try things, and figure it out together. Your work is saved right here.</p><button class="primary" id="new-main">＋ Start a lesson</button></div><div class="hero-robot">${ROBOT}</div></section>
  ${config.demo ? '<div class="demo-banner"><div><span class="tag tag--sun">offline demo</span><strong>Try Eve, one step at a time.</strong><p>Offline demo · built-in examples · no microphone, API calls, or handwriting recognition.</p></div><button id="walkthrough">▶ Play workflow demo</button></div>' : !config.ready ? `<div class="demo-banner">Live tutor needs ${escapeHTML(config.key_name)} in the bridge environment file. Restart the service after adding it. You can still save work.</div>` : ''}
  <section class="stats"><div class="stat"><strong>${rows.length}</strong><span>Saved lessons</span></div><div class="stat"><strong>${rows.filter(r=>r.stage==='complete').length}</strong><span>Problems completed</span></div><div class="stat"><strong>${new Set(rows.map(r=>r.topic.toLowerCase())).size}</strong><span>Topics explored</span></div></section>
  <div class="section-title"><h2>Your learning journey</h2><input id="search" placeholder="Search your lessons…" aria-label="Search lessons"></div><section class="cards" id="cards"></section>`;
  const cards = filter => {
    $('#cards').innerHTML=rows.filter(r=>(r.topic+' '+r.problem).toLowerCase().includes(filter.toLowerCase())).map(r=>{const [cls,expr]=art(r.topic);return `<article class="lesson-card"><div class="card-art ${cls}">${expr}</div><div class="card-body"><span class="badge">${readableStage(r.stage)}</span>${r.demo?'<span class="badge demo">DEMO</span>':''}<h3>${escapeHTML(r.topic)}</h3><p>${escapeHTML(r.problem || 'Bring your problem and a little curiosity.')}</p><span class="small">${escapeHTML(r.level)}</span></div><div class="card-footer"><span>${new Date(r.updated*1000).toLocaleDateString(undefined,{month:'short',day:'numeric'})} · ${r.mode==='learn'?'Practice':'Your problem'}</span><button data-open="${r.id}">${r.stage==='complete'?'Review':'Continue'} →</button></div></article>`}).join('') || '<div class="empty"><span class="kicker">nothing here yet</span><h3>Your next discovery belongs here.</h3><p>Start a lesson. Every sketch, idea, and helpful step will be saved.</p></div>';
    document.querySelectorAll('[data-open]').forEach(b=>b.onclick=()=>location.hash='lesson/'+b.dataset.open);
  }; cards(''); $('#search').oninput=e=>cards(e.target.value); $('#new-main').onclick=openNew;
  if ($('#walkthrough')) $('#walkthrough').onclick=()=>walkthrough().catch(fail);
  renderListen();
}
function openNew() { if (busy) return; $('#new-dialog').showModal(); $('#topic').focus(); say('Would you like to learn a topic or work on a problem you already have?'); }
async function openLesson(id) {
  clearTimeout(checkTimer); await flush(); lesson=await api('/api/action',{action:'select',id}); dirty=false; boardDirty=false; boardKey=null; lastRevision=lesson.revision; ideasBase=lesson.revision; renderLesson();
  const draft=localStorage.getItem('eve-draft-'+id);
  if (draft) { const parsed=JSON.parse(draft); if (parsed.revision===lesson.revision) { Object.assign(lesson,parsed.work); dirty=true; boardDirty='board' in parsed.work; boardKey=null; renderLesson(); status('Recovered unsaved work'); scheduleSave(); } }
}
function renderLesson() {
  const s=lesson; if(!s) return; $('#breadcrumb').textContent=s.topic;
  const input=['input','confirm'].includes(s.stage), done=s.stage==='complete', ended=s.stage==='ended';
  main.innerHTML=`<section class="lesson-head"><div><span class="kicker">${s.mode==='learn'?'learn a topic':'work through your problem'}${s.demo?' · offline demo':''}</span><h1>${escapeHTML(s.topic)}</h1><p>${escapeHTML(s.level)} <span class="slash">/</span> ${readableStage(s.stage)}</p></div><div class="toolbar-actions"><button id="history">Saved work</button><button id="export">Export</button><button id="finish">${ended?'Resume lesson':'Save & end'}</button></div></section>
  <div class="workspace"><section class="board-panel"><div class="problem-strip"><span class="kicker">${input?'your problem':'let’s try this'}</span><strong class="${(s.problem||'').length>160||(s.problem||'').includes('\n')?'long':''}">${escapeHTML(s.problem||'Write a problem or add a screenshot below.')}</strong></div>
  ${input?`<div class="input-area"><label for="upload" class="small">Upload a screenshot, or paste an image into this window</label><input id="upload" class="file-input" type="file" accept="image/png,image/jpeg,image/webp">${s.source_image?'<img id="source-preview" class="source-image" alt="Your original problem screenshot">':''}<label class="field">${s.stage==='confirm'?'Did Eve read this correctly? Edit any unclear symbols.':'Or type the problem'}<textarea id="problem-input" placeholder="Example: Solve 2x + 3 = 11">${escapeHTML(s.problem)}</textarea></label><p>${s.demo?'Demo does not recognize images. Type the problem to continue.':'Eve will read the problem first and ask you to confirm it.'}</p><button id="recognize">${s.stage==='confirm'?'Read again':'Read my problem'}</button>${s.stage==='confirm'?'<button id="confirm" class="primary">Yes, this is my problem</button>':''}</div>`:''}
  <div class="board-toolbar"><span>Your thinking goes here</span></div><div id="board-slot"></div>
  <div class="ideas-area"><label for="ideas">${s.mode==='help'?'How would you start? Show your ideas or tell Eve where you are stuck.':'Your ideas, working, or answer'}</label><textarea id="ideas" placeholder="I think the first thing to do is…">${escapeHTML(s.ideas)}</textarea></div>
  ${done?'<div class="completion"><h3>One more idea figured out. ✦</h3><p class="small">Review how you got here, then try something new.</p><button id="recap">Explain the method</button><button id="another" class="primary">Another problem</button><button id="change">Change topic</button></div>':''}
  ${s.stage==='setup'?'<div class="completion"><button id="generate" class="primary">Give me a problem</button></div>':''}</section>
  <aside class="tutor-panel"><div class="tutor-head">${FACE}<div><h3>Eve is here.</h3><p>Let's take it one step at a time.</p></div></div><p id="listen-banner" class="listen-banner hidden" aria-hidden="true"><span class="listen-dot"></span>eve is listening</p><div class="tutor-feed" id="feed"></div><div class="tutor-controls"><div class="listen-box"><button id="think-aloud" class="help help--listen" aria-pressed="false" aria-describedby="listen-note">◉ Think out loud</button><p id="listen-note" class="small listen-note"></p></div><button id="hint" class="help help--hint">✦ Give me a hint</button><button id="check" class="help help--check">✓ Check my work</button><button id="step" class="help help--step">→ Show one step</button><button id="stuck">I don't know how to start</button></div><label class="supervise"><input type="checkbox" id="supervise" ${supervise?'checked':''}>Check after a pause (5 seconds)</label><p class="small supervise">Your writing stays on the board. Eve's steps appear here.</p></aside></div>`;
  if ($('#source-preview')) $('#source-preview').src=s.source_image;
  renderFeed(); placeBoard();
  $('#ideas').oninput=()=>{lesson.ideas=$('#ideas').value; markDirty();};
  if ($('#problem-input')) $('#problem-input').oninput=()=>{lesson.problem=$('#problem-input').value;markDirty();};
  ['hint','check','step','recognize','confirm','generate','recap'].forEach(a=>{if($('#'+a)) $('#'+a).onclick=()=>act(a).catch(fail);});
  $('#stuck').onclick=async()=>{lesson.stuck=true;markDirty();await act('hint').catch(fail);};
  $('#finish').onclick=()=>act(ended?'resume':'end').catch(fail);
  $('#history').onclick=()=>showHistory().catch(fail); $('#export').onclick=()=>download().catch(fail);
  if($('#another')) $('#another').onclick=()=>createLesson({mode:'learn',topic:s.topic,level:s.level}).catch(fail);
  if($('#change')) $('#change').onclick=openNew;
  $('#supervise').onchange=e=>{supervise=e.target.checked;if(!supervise)clearTimeout(checkTimer);};
  $('#think-aloud').onclick=()=>toggleListen();
  if($('#upload')) $('#upload').onchange=e=>upload(e.target.files[0]).catch(fail);
  if(input || done || ended || s.stage==='setup') ['hint','check','step','stuck'].forEach(id=>$('#'+id).disabled=true);
  if(ended)$('#ideas').disabled=true;
  status('Saved locally');
  renderListen();
}
/* Think out loud: the toggle, the note under it, the panel banner and the header pill, all from one state. */
function renderListen() {
  const on=listening.state==='on', starting=listening.state==='starting', active=on||starting;
  const text=on?'eve is listening':starting?'Getting eve’s ears ready…':'';
  const pill=$('#listen-pill');
  if(pill.textContent!==text){pill.innerHTML=text?`<span class="listen-dot" aria-hidden="true"></span>${escapeHTML(text)}`:'';}
  pill.classList.toggle('hidden',!text); pill.classList.toggle('listen-pill--on',on);
  const button=$('#think-aloud'); if(!button||!lesson) return;
  const here=!listening.lesson_id||listening.lesson_id===lesson.id, ready=['working','complete'].includes(lesson.stage);
  button.setAttribute('aria-pressed',String(active));
  button.textContent=active?'■ Stop listening':'◉ Think out loud';
  button.disabled=listenBusy||(!active&&(!listening.available||!ready));
  $('#listen-note').textContent=!listening.available?(listening.reason||'Think out loud needs the eve robot app running.')
    :active?(here?'Talk through your thinking. eve saves what you say to your ideas. Say “I’m done” to finish.':'eve is listening to another lesson. Stop it here first.')
    :!ready?(lesson.stage==='ended'?'Resume this lesson to think out loud.':'Get your problem ready, then think out loud.')
    :'eve listens with the microphone only while this is on. Nothing is recorded.';
  $('#listen-banner').classList.toggle('hidden',!on);
}
function setListening(result) { listening={available:!!result.available,state:result.state||'off',lesson_id:result.lesson_id||null,reason:result.reason||''}; renderListen(); }
async function pollListen() { try { const fresh=await api('/api/listening'); if(JSON.stringify(fresh)!==JSON.stringify(listening)) setListening(fresh); } catch { /* Keep the last state during a transient disconnect. */ } }
async function toggleListen() {
  if(listenBusy||!lesson) return; clearError();
  const stop=listening.state!=='off'; listenBusy=true; renderListen();
  try { if(!stop) await flush(); setListening(await api('/api/action',{action:stop?'stop-listening':'listen',id:lesson.id})); if(!stop) say('I am listening.'); }
  catch(e) { fail(e); await pollListen(); }
  finally { listenBusy=false; renderListen(); }
}
function renderReferences(event) {
  const sources = (event.sources || []).filter(s => {
    try { const u = new URL(s.url); return u.protocol === 'https:' && !u.username && !u.password; }
    catch { return false; }
  });
  return `${event.search_note ? `<p>${escapeHTML(event.search_note)}</p>` : ''}${sources.length ? `<details><summary>Practice references (may include answers)</summary><ul>${sources.map(s => `<li><a href="${escapeHTML(s.url)}" target="_blank" rel="noopener noreferrer">${escapeHTML(s.title)}</a></li>`).join('')}</ul></details>` : ''}`;
}
function renderFeed() {
  const feed=$('#feed'); if(!feed)return;
  feed.innerHTML=lesson.events.filter(e=>e.feedback||e.step).map(e=>`<div class="bubble"><span class="kicker">${escapeHTML(FEED_LABEL[e.action] || e.action)}</span>${e.step?`<strong class="math-step">${escapeHTML(e.step)}</strong>`:''}${escapeHTML(e.feedback)}${renderReferences(e)}</div>`).join('') || `<div class="bubble">${lesson.mode==='help'?'First, add your problem. Then write your ideas—even a small start helps.':'Take your time. Try an idea, draw it out, or ask me for a hint.'}</div>`;
  feed.scrollTop=feed.scrollHeight;
}
/* The whiteboard is tldraw. One editor lives as long as the page: every renderLesson() rebuilds main, so the editor's host node
   is kept here and moved back into each fresh render. boardKey is the lesson whose board the editor holds; null means "load it". */
const boardHost=document.createElement('div');boardHost.id='board';boardHost.className='board-host';
boardHost.setAttribute('aria-label','Whiteboard. Draw, write or type on it, or use the ideas text box below.');
/* The server puts a fresh nonce on this script tag for every page load; tldraw stamps it on the <style> elements it adds. */
const PAGE_NONCE=document.querySelector('script[src="/app.js"]')?.nonce||'';
let board=null, boardMount=null, boardKey=null, boardDirty=false;
const LEGACY_BOARD={width:1200,height:775};
function placeBoard(){
  const slot=$('#board-slot');if(!slot)return;
  if(!window.EveCanvas){slot.className='board-host unavailable';slot.textContent='The whiteboard is not built yet. In bridge/web-canvas run “npm install”, then “npm run build”, and reload. Your ideas box and eve’s help still work.';return;}
  slot.replaceWith(boardHost);
  boardMount??=window.EveCanvas.mount(boardHost,{licenseKey:config.tldraw_license_key,nonce:PAGE_NONCE,onChange:()=>{if(!lesson||boardKey!==lesson.id)return;boardDirty=true;markDirty();}}).then(handle=>{board=handle;});
  boardMount.then(syncBoard).catch(fail);
}
/* A lesson saved before tldraw has pen strokes and their flattened picture. The picture goes on the board, locked, exactly as the
   learner left it (erased parts stay erased); new work goes on top. */
function syncBoard(){
  if(!board||!lesson)return;
  if(boardKey!==lesson.id){boardKey=lesson.id;const old=!lesson.board&&lesson.strokes?.length&&lesson.board_image;board.load(lesson.board||null,old?{image:lesson.board_image,...LEGACY_BOARD}:null);}
  board.setReadOnly(busy||lesson.stage==='ended');
}
const interacting=()=>!!board&&board.isInteracting();
const marks=r=>Object.values(r.board?.store||{}).filter(x=>x.typeName==='shape').length+(r.strokes||[]).length;
/* What a save sends. The board and its picture for the tutor are sent only when the learner changed the board, so a lesson whose
   board was never touched here (or whose whiteboard could not load) keeps what it has. */
async function work(withBoard){
  if(withBoard&&boardMount){await boardMount.catch(()=>{});syncBoard();}/* A recovered draft can be saved before the editor is up. */
  if(withBoard&&board&&boardKey===lesson.id){lesson.board=board.snapshot();lesson.board_image=await board.image();}
  const w={ideas:lesson.ideas,problem:lesson.problem,stuck:lesson.stuck,source_image:lesson.source_image};
  if(withBoard){w.board=lesson.board??null;w.board_image=lesson.board_image||'';}
  return w;
}
/* The recovery draft holds the board itself, not its picture: the picture is made again at the next save. */
function saveDraft(){if(!lesson)return;const w={ideas:lesson.ideas,problem:lesson.problem,stuck:lesson.stuck,source_image:lesson.source_image};if(boardDirty&&board&&boardKey===lesson.id)w.board=board.snapshot();try{localStorage.setItem('eve-draft-'+lesson.id,JSON.stringify({revision:lesson.revision,work:w}));}catch{/* Server persistence remains primary when browser quota is full. */}}
function markDirty(){dirty=true;status('Saving…');clearTimeout(draftTimer);draftTimer=setTimeout(saveDraft,250);scheduleSave();clearTimeout(checkTimer);if(supervise&&lesson.stage==='working')checkTimer=setTimeout(()=>{if(!busy&&!interacting())act('check').catch(fail);},5000);}
function scheduleSave(){clearTimeout(saveTimer);saveTimer=setTimeout(()=>flush().catch(fail),650);}
let saving=Promise.resolve();
/* A save can come back with more ideas than it sent: lines eve heard while the learner typed. Show them unless the learner
   typed again meanwhile; then keep ideasBase old, so the next save asks the server to put them back again. */
function flush(){clearTimeout(saveTimer);saving=saving.catch(()=>{}).then(async()=>{if(!dirty||!lesson)return;const current=lesson;const withBoard=boardDirty;dirty=false;boardDirty=false;saveInFlight=true;let payload;try{payload=await work(withBoard);const saved=await api('/api/action',{action:'save',id:current.id,revision:current.revision,ideas_base:ideasBase,work:payload});if(lesson===current){lesson.revision=saved.revision;lastRevision=saved.revision;const box=$('#ideas');if(saved.ideas===payload.ideas){ideasBase=saved.revision;}else if(!dirty&&(!box||box.value===payload.ideas)){lesson.ideas=saved.ideas;if(box)box.value=saved.ideas;ideasBase=saved.revision;}if(!dirty){clearTimeout(draftTimer);localStorage.removeItem('eve-draft-'+current.id);status('Saved locally');}else scheduleSave();}}catch(e){dirty=true;boardDirty=boardDirty||withBoard;status('Not saved — retry needed');throw e;}finally{saveInFlight=false;}});return saving;}
async function act(action){if(busy||!lesson)return;clearError();clearTimeout(checkTimer);setBusy(true);try{await flush();status(action==='step'?'Eve is working on one step…':'Eve is thinking…');lesson=await api('/api/action',{action,id:lesson.id,revision:lesson.revision});lastRevision=lesson.revision;ideasBase=lesson.revision;renderLesson();const last=lesson.events.at(-1);if(last)say([last.step,last.feedback].filter(Boolean).join('. '));}finally{setBusy(false);if(lesson)renderLesson();}}
async function createLesson(data){clearTimeout(checkTimer);await flush();setBusy(true);try{lesson=await api('/api/action',{action:'create',...data});dirty=false;boardDirty=false;boardKey=null;ideasBase=lesson.revision;historyReplace(lesson.id);renderLesson();}finally{setBusy(false);if(lesson)renderLesson();}if(lesson.mode==='learn')await act('generate');}
function historyReplace(id){window.history.replaceState(null,'','#lesson/'+id);}
async function upload(file){if(!file||!lesson||!['input','confirm'].includes(lesson.stage))return;if(!/^image\/(png|jpeg|webp)$/.test(file.type)||file.size>10*1024*1024)throw new Error('Choose a PNG, JPEG or WebP smaller than 10 MB.');const image=await createImageBitmap(file);const c=document.createElement('canvas');const scale=Math.min(1,1600/Math.max(image.width,image.height));c.width=image.width*scale;c.height=image.height*scale;c.getContext('2d').drawImage(image,0,0,c.width,c.height);image.close();lesson.source_image=c.toDataURL('image/jpeg',.9);markDirty();await flush();renderLesson();}
document.addEventListener('paste',e=>{if(boardHost.contains(document.activeElement))return;/* The whiteboard takes its own pastes. */const file=[...(e.clipboardData?.items||[])].find(x=>x.type.startsWith('image/'))?.getAsFile();if(file&&lesson&&['input','confirm'].includes(lesson.stage)){e.preventDefault();upload(file).catch(fail);}});
async function showHistory(){await flush();const rows=await api('/api/lessons/'+lesson.id+'/history');$('#history-list').innerHTML=rows.slice().reverse().map(r=>`<details class="history-row"><summary>Revision ${r.revision} · ${new Date(r.updated*1000).toLocaleTimeString()} · ${readableStage(r.stage)}</summary><p>${escapeHTML(r.problem)}</p><pre>${escapeHTML(r.ideas||'No typed ideas in this revision.')}</pre>${r.source_image?`<img src="${r.source_image}" alt="Original screenshot">`:''}${r.board_image?`<img src="${r.board_image}" alt="Saved whiteboard revision ${r.revision}">`:''}<p>${marks(r)} marks on the board · ${r.events.length} tutor events</p></details>`).join('');$('#history-dialog').showModal();}
async function download(){await flush();const revisions=await api('/api/lessons/'+lesson.id+'/history');const url=URL.createObjectURL(new Blob([JSON.stringify({lesson,revisions},null,2)],{type:'application/json'}));const a=document.createElement('a');a.href=url;a.download='eve-lesson-'+lesson.id+'.json';a.click();setTimeout(()=>URL.revokeObjectURL(url),1000);}
const pause=ms=>new Promise(r=>setTimeout(r,ms));
async function walkthrough(){if(!config.demo||tour)return;tour=true;try{await createLesson({mode:'learn',topic:'Algebra — balancing equations',level:'Grades 6–8'});await pause(1500);lesson.ideas='I think I should subtract 3 from both sides.';$('#ideas').value=lesson.ideas;markDirty();await flush();await act('hint');await pause(1600);for(let i=0;i<3;i++){await act('step');await pause(1700);}await act('recap');await pause(1500);location.hash='';}finally{tour=false;}}
$('#new-side').onclick=openNew;$('#close-dialog').onclick=()=>$('#new-dialog').close();$('#close-history').onclick=()=>$('#history-dialog').close();
$('#new-form').onsubmit=async e=>{e.preventDefault();const mode=new FormData(e.target).get('mode');const topic=$('#topic').value.trim();if(mode==='learn'&&!topic){$('#topic').focus();return;}$('#new-dialog').close();await createLesson({mode,topic,level:$('#level').value}).catch(fail);};
$('#speak-toggle').onclick=()=>{speak=!speak;$('#speak-toggle').textContent='Read aloud: '+(speak?'on':'off');$('#speak-toggle').setAttribute('aria-pressed',speak);if(!speak&&'speechSynthesis'in window)speechSynthesis.cancel();};
window.addEventListener('beforeunload',e=>{if(dirty||busy){e.preventDefault();e.returnValue='';}});
async function route(){clearError();if(busy)return;const id=location.hash.startsWith('#lesson/')?location.hash.slice(8):null;if(id)await openLesson(id);else await dashboard();}
window.addEventListener('hashchange',()=>route().catch(fail));
/* A spoken idea lands as a new revision. While the learner's cursor is in the ideas box, update the text in place instead of
   redrawing the page, so the caret and focus stay where they are. */
function applyFresh(fresh){if(JSON.stringify(fresh.board??null)!==JSON.stringify(lesson.board??null))boardKey=null;const box=$('#ideas');const focused=box&&document.activeElement===box&&fresh.stage===lesson.stage&&fresh.events.length===lesson.events.length;lesson=fresh;lastRevision=fresh.revision;ideasBase=fresh.revision;if(focused){syncBoard();const end=box.selectionStart===box.value.length;const at=box.selectionStart;box.value=fresh.ideas;if(end)box.selectionStart=box.selectionEnd=box.value.length;else box.selectionStart=box.selectionEnd=Math.min(at,box.value.length);renderListen();}else renderLesson();}
(async()=>{config=await api('/api/config');$('#mode-label').textContent=config.demo?'OFFLINE DEMO':`${config.provider.toUpperCase()} · ${config.model}`;listening.available=!!config.listen_available;await pollListen();await route();setInterval(pollListen,1500);setInterval(async()=>{if(!lesson||busy||dirty||interacting()||saveInFlight)return;try{const fresh=await api('/api/lessons/'+lesson.id);if(fresh.revision>lastRevision&&!dirty&&lesson.id===fresh.id)applyFresh(fresh);}catch{/* Keep work visible during transient disconnects. */}},3000);})().catch(fail);
