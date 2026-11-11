import {cleanHtml,textHtml,plainText,setupEditor} from './compose';
import {icon,type IconName} from './icons';
import {createMailFrame} from './frame';
import {dateLabels} from './dates';
import type {Signature,Message,Draft,DraftFile,DraftRecord,Snapshot,Action,AccountStatus,SyncHistory,DeviceAuthorization,MailTarget,BulkOutcome} from './types';
interface Responses {
 signature:Signature;set_signature:null;
 bulk_modify:BulkOutcome;remove_account:string;set_sync_history:string;account_authorization:DeviceAuthorization|null;
 sync_status:AccountStatus[];pause_sync:string;remote_images:{index:number;data_url:string|null;error:string|null}[];
 snapshot:Snapshot;mailbox:{messages:Message[];total:number;page_size:number};cached_message:Message;content:Message;
 new_draft:DraftRecord;load_draft:DraftRecord;attach:DraftRecord;remove_attachment:DraftRecord;forward_attachments:DraftRecord;
 save_draft:null;delete_draft:null;notifications:null;open_link:null;sync:string;send:string;modify:string;download:string;connect:string;connect_profile:string;reconnect:string;account_profile:Record<string,string|number|boolean>|null;
}
// Svelte owns the shell; this controller keeps the established Rust mail bridge.
export function mountMailClient(){
const $ = <T extends HTMLElement = HTMLElement>(id:string) => document.getElementById(id)! as T;
const invoke = window.__TAURI__?.core.invoke;
let snapshot:Snapshot = {messages:[],accounts:[],drafts:[]};
const expandedAccounts=new Set<string>();
const accountFolder=(account:string,name:string)=>'account-folder:'+JSON.stringify([account,name]);
const folderScope=():[string,string]|null=>folder.startsWith('account-folder:')?JSON.parse(folder.slice(15)):null;
const isDrafts=()=>folder==='Drafts'||folderScope()?.[1]==='Drafts';
let folder='All Inboxes',selected:Message|null=null;
const tabs:Message[]=[];
const checked=new Map<string,MailTarget>();
let removingAccount:string|null=null;
let lastBulkResult='';
let poll:number|undefined,polling=false,lastMailboxReload=0,statusUntil=0;
let lastRendered:Message|null=null,disposeFrame:(()=>void)|undefined,imagesLoading=false;
let pendingImageMessage:Message|null=null;
const remoteImages=new Map<string,Map<number,string>>();
const messageKey=(m:Message)=>JSON.stringify([m.account,m.id]);
let disposed=false,interval:number|undefined,unlisten:(()=>void)|undefined;
let progressListenersReady:Promise<void>|null=null,signInActive=false,signInExpired=false,signInCancelling=false;
let signInExpiresAt=0,signInTimer:number|undefined,signInBrowserUrl='';
let signatureAccount='';
let draft:Draft|null=null,draftFiles:DraftFile[]=[],pendingLink='',busy=false,dirty=false;
let offset=0, total=0, pageSize=100, viewRevision=0, renderedViewRevision=-1;
let saveTimer:number|undefined,saving:Promise<unknown>=Promise.resolve(),loading=false;
const el = <K extends keyof HTMLElementTagNameMap>(tag:K,text?:string,className?:string):HTMLElementTagNameMap[K] => {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
};
const button = (text:string,action:()=>unknown,className?:string) => {
  const node = el('button', text, className);
  node.type = 'button';
  node.addEventListener('click', () => safely(action));
  return node;
};
const status = (text:string) => { if(!disposed){$('status').textContent=text;statusUntil=Date.now()+10000;} };
function clearSignInCode(){
 signInExpiresAt=0;clearInterval(signInTimer);signInTimer=undefined;
 $<HTMLInputElement>('account-signin-code').value='';$('account-signin-code-area').hidden=true;
 signInBrowserUrl='';$('account-signin-browser-area').hidden=true;
}
function showSignInCode(prompt:DeviceAuthorization){
 if(!signInActive||signInExpired||signInCancelling)return;
 if(prompt.verification_uri){
   const url=new URL(prompt.verification_uri);
   if(url.protocol!=='https:'||url.hostname!=='login.microsoftonline.com'||url.username||url.password||!url.pathname.endsWith('/oauth2/v2.0/authorize'))return;
   clearSignInCode();signInBrowserUrl=url.href;$('account-signin-browser-area').hidden=false;
   $('account-signin-status').textContent='Finish Microsoft sign-in in your browser. No device code is needed.';
   return;
 }
 if(!prompt.user_code||prompt.user_code.length>64)return;
 $<HTMLInputElement>('account-signin-code').value=prompt.user_code;
 $('account-signin-code-area').hidden=false;
 $('account-signin-status').textContent='Microsoft is waiting for you to sign in.';
 signInExpiresAt=Date.now()+Math.max(1,Math.min(900,prompt.expires_in))*1000;
 clearInterval(signInTimer);
 const tick=()=>{
   const seconds=Math.max(0,Math.ceil((signInExpiresAt-Date.now())/1000));
   $('account-signin-expiry').textContent=`Code expires in ${Math.floor(seconds/60)}:${String(seconds%60).padStart(2,'0')}`;
   if(!seconds){signInExpired=true;clearSignInCode();$('account-signin-status').textContent='This code expired. Close this dialog and connect again for a new code.';safely(()=>invoke?.('cancel_operation'));}
 };
 tick();signInTimer=window.setInterval(tick,1000);
}
function ensureProgressListeners():Promise<void>{
 if(!progressListenersReady)progressListenersReady=(async()=>{
   if(!window.__TAURI__)throw new Error('Open FocalMail in its desktop window.');
   const offText=await window.__TAURI__.event.listen('mail-progress',event=>{
     status(event.payload);
     if(signInActive){
       if(event.payload.startsWith('Microsoft account authorized')||event.payload.startsWith('Microsoft browser response received'))clearSignInCode();
       $('account-signin-status').textContent=event.payload;
     }
   });
   try{
     const offCode=await window.__TAURI__.event.listen<DeviceAuthorization>('mail-device-authorization',event=>showSignInCode(event.payload));
     const off=()=>{offText();offCode();};if(disposed)off();else unlisten=off;
   }catch(error){offText();throw error;}
 })().catch(error=>{progressListenersReady=null;throw error;});
 return progressListenersReady;
}
async function runAccountConnection<T>(fn:()=>Promise<T>,microsoft:boolean){
 if(busy||signInActive)return;
 if(!microsoft)return operation(fn);
 signInActive=true;signInExpired=false;signInCancelling=false;clearSignInCode();
 $('account-signin-status').textContent='Starting Microsoft sign-in…';
 $('cancel-account-signin').hidden=false;$('close-account-signin').hidden=true;
 const dialog=$<HTMLDialogElement>('account-signin');if(!dialog.open)dialog.showModal();
 try{
   await ensureProgressListeners();
   const result=await operation(fn);
   clearSignInCode();$('account-signin-status').textContent=typeof result==='string'?result:'Account connected.';
   return result;
 }catch(error){
   clearSignInCode();
   if(!signInExpired)$('account-signin-status').textContent=signInCancelling?'Sign-in cancelled. Connect again when you are ready.':String(error);
   throw error;
 }finally{signInActive=false;$('cancel-account-signin').hidden=true;$('close-account-signin').hidden=false;}
}
async function cancelAccountSignIn(){
 if(!signInActive)return;
 signInCancelling=true;clearSignInCode();$('account-signin-status').textContent='Cancelling sign-in…';
 await invoke?.('cancel_operation');
}
async function api<K extends keyof Responses>(op:K,args:Record<string,unknown>={}) {
  if (!invoke) throw new Error('Open FocalMail in its Tauri desktop window.');
  return invoke<Responses[K]>('mail_request', {request:{op,...args}});
}
async function safely(fn:()=>unknown) {
  try { return await fn(); } catch (e) { status(String(e)); return undefined; }
}
async function operation<T>(fn:()=>Promise<T>) {
  if (busy) return;
  busy = true;
  $('cancel').hidden = false;
  document.querySelectorAll<HTMLInputElement>('button, input, select, textarea').forEach(b => { if (!['cancel','search','settings','close-settings','previous-page','next-page','filter-all','filter-unread','filter-starred','account-signin-code','copy-signin-code','open-microsoft-signin','open-microsoft-browser','cancel-account-signin','close-account-signin'].includes(b.id) && !b.closest('nav') && !b.classList.contains('message') && !b.closest('#message-tabs')) b.disabled = true; });
  try { const result = await fn(); if (typeof result === 'string') status(result); await reload(); return result; }
  finally {
    busy = false;
    $('cancel').hidden = true;
    document.querySelectorAll<HTMLInputElement>('button, input, select, textarea').forEach(b => { b.disabled = false; });
    updatePagination();updateActions();renderSyncStatus();renderSelection();
  }
}
function same(a:Message|null,b:Message|null) { return a && b && a.account === b.account && a.id === b.id; }
const folderIcons:Record<string,IconName>={'All Inboxes':'inbox',Sent:'send',Drafts:'edit',Archive:'archive',Spam:'shield',Trash:'trash','Server Drafts':'file',Unread:'unread',Starred:'star'};
function folderButton(name:string){
 const b=button('',()=>changeFolder(name),`folder ${folder===name?'active':''}`);
 b.setAttribute('aria-current',folder===name?'page':'false');
 b.append(icon(folderIcons[name]||'mail'),el('span',name==='All Inboxes'?'Inbox':name));
 if(name==='Drafts' && snapshot.drafts.length)b.append(el('span',String(snapshot.drafts.length),'badge'));
 return b;
}
function renderFolders() {
 $('folders').replaceChildren(...['All Inboxes','Sent','Drafts','Trash','Spam','Archive','Server Drafts'].map(folderButton));
 $('smart-folders').replaceChildren(...['Starred','Unread'].map(folderButton));
 $('accounts').replaceChildren(...snapshot.accounts.map(a=>{
   const group=el('div',undefined,'account-group');
   const scope=folderScope();
   const open=expandedAccounts.has(a);
   const b=button('',()=>{if(open)expandedAccounts.delete(a);else expandedAccounts.add(a);renderFolders();},`folder account ${scope?.[0]===a?'active':''}`);
   b.setAttribute('aria-expanded',String(open));
   const provider=snapshot.status?.find(s=>s.email===a)?.provider;
   const mark=el('span',provider==='gmail'?'G':provider==='outlook'?'M':'@',`provider provider-${provider||'imap'}`);
   b.append(mark,el('span',a,'account-address'),icon('chevron'));b.title=a;group.append(b);
   if(open){
     const names=[...new Set(['Inbox','Sent','Drafts','Archive','Spam','Trash','Server Drafts',...(snapshot.folders?.[a]||[])])];
     for(const name of names){
       const key=accountFolder(a,name);
       const child=button('',()=>changeFolder(key),`folder account-folder ${folder===key?'active':''}`);
       child.style.paddingLeft='32px';child.title=`${a} / ${name}`;
       child.setAttribute('aria-current',folder===key?'page':'false');
       child.append(icon(folderIcons[name]||'mail'),el('span',name));group.append(child);
     }
   }
   return group;
 }));
 if(!snapshot.accounts.length)$('accounts').append(button('Add your first account',()=>openSettings(),'add-first'));
 for(const [id,target] of [['filter-all','All Inboxes'],['filter-unread','Unread'],['filter-starred','Starred']]){
   $(id).classList.toggle('active',folder===target);$(id).setAttribute('aria-pressed',String(folder===target));
 }
 $('connection-dot').dataset.state=snapshot.status?.some(s=>s.state!=='connected')?'warning':snapshot.accounts.length?'connected':'offline';
}
function initials(m:Message){return (m.sender||m.email||'?').split(/\s+/).slice(0,2).map(s=>s[0]).join('').toUpperCase();}
function avatar(m:Message){const tone=[...(m.email||m.sender)].reduce((n,c)=>n+c.charCodeAt(0),0)%6;return el('span',initials(m),`avatar tone-${tone}`);}
function renderTabs(){
 $('message-tabs').replaceChildren(...tabs.map((m,index)=>{
   const tab=el('div',undefined,`message-tab ${same(m,selected)?'active':''}`);
   const select=button(m.subject||'(No subject)',()=>openMessage(m),'tab-select');
   select.setAttribute('role','tab');select.setAttribute('aria-selected',String(!!same(m,selected)));select.setAttribute('aria-controls','reader');select.id=`message-tab-${index}`;
   select.onkeydown=e=>{if(e.key==='ArrowRight'||e.key==='ArrowLeft'){e.preventDefault();const next=(index+(e.key==='ArrowRight'?1:-1)+tabs.length)%tabs.length;safely(()=>openMessage(tabs[next]));$('message-tabs').querySelectorAll<HTMLButtonElement>('.tab-select')[next]?.focus();}};
   const close=button('',()=>{tabs.splice(index,1);remoteImages.delete(messageKey(m));if(same(m,selected))selected=tabs[Math.min(index,tabs.length-1)]||null;render();},'tab-close');close.append(icon('close'));close.setAttribute('aria-label',`Close ${m.subject||'message'}`);
   tab.append(select,close);return tab;
 }));
 $('message-tabs').classList.toggle('no-tabs',!tabs.length);
 if(!tabs.length)$('message-tabs').append(el('span','MESSAGE READER','reader-caption'));
 const active=tabs.findIndex(m=>same(m,selected));
 if(active>=0)$('reader').setAttribute('aria-labelledby',`message-tab-${active}`);else $('reader').removeAttribute('aria-labelledby');
}
async function openMessage(m:Message){
 const existing=tabs.find(t=>same(t,m));selected=existing||m;
 if(!existing){if(tabs.length>=12){const evicted=tabs.shift();if(evicted)remoteImages.delete(messageKey(evicted));}tabs.push(m);}
 render();
 const full=await api('cached_message',{account:m.account,id:m.id});
 const index=tabs.findIndex(t=>same(t,m));if(index>=0)tabs[index]=full;
 if(same(selected,m)){selected=full;renderReader();updateActions();}
}
function updateActions(){
 for(const id of ['reply','reply-all','forward','archive','trash','mark-read'])$<HTMLButtonElement>(id).disabled=!selected||!!selected.pending_move||busy;
 const text=$('mark-read').querySelector('span');if(text)text.textContent=selected?.unread?'Mark read':'Mark unread';
}
async function modify(action:Action){
 if(!selected)return;
 const m=selected;
 await operation(async()=>{
   const result=await api('modify',{account:m.account,id:m.id,action});
   const full=await api('cached_message',{account:m.account,id:m.id});
   const index=tabs.findIndex(t=>same(t,m));if(index>=0)tabs[index]=full;
   if(same(m,selected))selected=full;return result;
 });
}
function openSettings(){renderAccounts();$<HTMLDialogElement>('preferences').showModal();}
function render() {
  renderFolders(); $('folder-title').textContent=folderScope()?.join(' / ')||folder;
  const list = $('messages'); list.replaceChildren();
  if (isDrafts()) {
    $('count').textContent=String(snapshot.drafts.filter(d=>!folderScope()||d.from===folderScope()![0]).length);
    for (const d of snapshot.drafts.filter(d=>!folderScope()||d.from===folderScope()![0])) {
      const b=button('',async()=>{await flush();await openDraft(await api('load_draft',{id:d.id}));},'message');
      b.append(el('div',d.subject || '(No subject)','subject'),el('div',d.to || 'No recipients','snippet'));
      list.append(b);
    }
  } else {
    const messages = snapshot.messages;
    $('count').textContent=total ? `${offset+1}–${offset+messages.length} of ${total}` : '0';
    let previousDay='';
    for (const m of messages) {
      const date=dateLabels(m);
      if(date.key!==previousDay){list.append(el('h2',date.group,'date-divider'));previousDay=date.key;}
      const b=button('',()=>openMessage(m),`message ${m.unread?'unread':''} ${same(m,selected)?'selected':''}`);
      const row=el('div',undefined,'row');const time=el('time',date.short,'date');time.title=date.full;if(date.iso)time.dateTime=date.iso;row.append(el('span',m.sender || m.email,'sender'),time);
      const content=el('div',undefined,'message-copy');content.append(row,el('div',m.subject || '(No subject)','subject'),el('div',m.body.slice(0,160),'snippet'));b.append(avatar(m),content);if(m.flagged){const star=icon('star');star.classList.add('message-star');b.append(star);}
      const wrapper=el('div',undefined,`message-row ${checked.has(messageKey(m))?'checked':''}`);
      const check=el('input');check.type='checkbox';check.className='message-check';check.checked=checked.has(messageKey(m));check.disabled=busy||!!m.pending_move;if(m.pending_move)check.title='Folder refresh pending';check.setAttribute('aria-label',`Select ${m.subject||'message'} from ${m.sender||m.email}`);
      check.addEventListener('change',()=>{if(check.checked&&checked.size>=100){check.checked=false;status('Select up to 100 messages at a time.');return;}if(check.checked)checked.set(messageKey(m),{account:m.account,id:m.id});else checked.delete(messageKey(m));wrapper.classList.toggle('checked',check.checked);renderSelection();});
      wrapper.append(check,b);list.append(wrapper);
    }
    if (!messages.length) list.append(el('div',snapshot.accounts.length ? 'No messages here.' : 'Add an account in Settings to get started.','empty'));
  }
  updatePagination();
  renderTabs();renderReader();updateActions();renderSelection();renderSearchStatus();
}
function updatePagination() {
  $('pagination').hidden=isDrafts() || total<=pageSize;
  $<HTMLButtonElement>('previous-page').disabled=offset===0;
  $<HTMLButtonElement>('next-page').disabled=offset+pageSize>=total;
}
async function changeFolder(name:string) {
  folder=name;const scope=folderScope();if(scope)expandedAccounts.add(scope[0]);offset=0;viewRevision++;checked.clear();lastBulkResult='';await reload();
}
let formatted=true;
function renderReader(force=false) {
  const r=$('reader');
  if(!force && lastRendered===selected && r.childElementCount)return;
  lastRendered=selected;disposeFrame?.();disposeFrame=undefined;r.replaceChildren();
  if(!selected){
    const empty=el('div',undefined,'reader-empty');const tile=el('div',undefined,'empty-icon');tile.append(icon('mail'));
    empty.append(tile,el('h1','Your inbox, in focus.'),el('p','Choose a message to read, or start a conversation.'),button('New message',()=>compose(),'primary'));
    r.append(empty);return;
  }
  const m=selected;
  const header=el('div',undefined,'reader-header');
  const identity=el('div',undefined,'sender-identity');identity.append(el('strong',m.sender||m.email),el('span',m.email,'muted'));
  const star=button('',()=>modify({Star:!m.flagged}),`reader-star icon-button ${m.flagged?'starred':''}`);star.append(icon('star'));star.setAttribute('aria-label',m.flagged?'Unstar message':'Star message');star.disabled=busy||!!m.pending_move;
  header.append(avatar(m),identity,star);r.append(header);
  const meta=el('div',undefined,'metadata');
  const field=(label:string,value:string)=>{const row=el('div');row.append(el('span',label,'meta-label'),el('span',value));return row;};
  meta.append(field('To',m.to_header||m.account));if(m.cc_header)meta.append(field('Cc',m.cc_header));const dated=field('Date',dateLabels(m).full);dated.title=`Original sender date: ${m.date||'Unavailable'}`;meta.append(dated);
  r.append(meta,el('h1',m.subject||'(No subject)','reader-subject'));
  if(m.pending_move)r.append(el('p','This message has moved on the server. Its folder identity will update after sync; cached content remains readable.','muted'));
  const privacy=el('div',undefined,'privacy-banner');const loadedImages=remoteImages.get(messageKey(m))?.size||0;privacy.append(icon('shield'),el('span',loadedImages?`${loadedImages} remote images loaded for this message`:'Remote content blocked by default'));
  if(m.html)privacy.append(button(formatted?'Plain text':'Formatted',()=>{formatted=!formatted;renderReader(true);},'format-toggle'));
  const remoteCount=(m.html?.images||[]).filter(i=>i.source?.startsWith('https://')).length;
  if(remoteCount){
    const loaded=remoteImages.get(messageKey(m))?.size||0;
    privacy.append(button(imagesLoading?'Loading images…':loaded?'Reload images':`Load images (${Math.min(remoteCount,20)})`,()=>{pendingImageMessage=m;$<HTMLDialogElement>('remote-images-confirm').showModal();},'load-images'));
    if(imagesLoading)privacy.querySelectorAll('button.load-images').forEach(b=>(b as HTMLButtonElement).disabled=true);
  }
  r.append(privacy);
  if (!m.html_loaded || !m.attachments_loaded || (m.html && (!m.html.layout || (!m.html.inline_attempted&&m.html.images?.some(i=>i.source?.startsWith('cid:')&&!i.inline_data))))) r.append(button('Load full message and inline images',()=>operation(async()=>{
    const content=await api('content',{account:m.account,id:m.id}); remoteImages.delete(messageKey(m));selected=content;const tabIndex=tabs.findIndex(t=>same(t,m));if(tabIndex>=0)tabs[tabIndex]=content;
    const index=snapshot.messages.findIndex(x=>same(x,m));if(index>=0)snapshot.messages[index]=content;renderReader();
  })));
  const body=el('div',undefined,'message-body');
  if(formatted && m.html?.layout){
    const view=createMailFrame(m,remoteImages.get(messageKey(m))||new Map(),previewLink);
    body.append(view.frame);disposeFrame=view.dispose;
  } else if (formatted && m.html) {
    body.classList.add('formatted');
    for (const block of m.html.blocks) {
      const p=el('p');
      for (const span of block.spans) {
        let node:HTMLElement=span.link?button(span.text,()=>previewLink(span.link!),'link'):el(span.code?'code':'span',span.text);
        if(span.heading)node.classList.add('heading');
        if(span.bold){const b=el('strong');b.append(node);node=b;}
        if(span.italic){const i=el('em');i.append(node);node=i;}p.append(node);
      }
      body.append(p);
    }
    if(m.html.blocked_images)body.append(el('p','Images blocked to protect your privacy.','muted'));
    if(m.html.limited)body.append(el('p','Formatted content was truncated. Switch to plain text for the available message.','muted'));
  } else body.textContent=m.body;
  r.append(body);
  (m.attachments||[]).forEach((f,index)=>r.append(button(`Save to Downloads: ${f.name} (${Math.ceil(f.size/1024)} KiB)`,()=>operation(()=>api('download',{account:m.account,id:m.id,index})))));
}
async function reload() {
  if(loading)return; loading=true;
  const revision=viewRevision;
  try {
    const next=await api('snapshot');
    const page=isDrafts() ? {messages:[],total:0,page_size:100} : await api('mailbox',{folder,query:$<HTMLInputElement>('search').value,offset});
    if(revision!==viewRevision)return;
    next.messages=page.messages;
    const changed=renderedViewRevision!==revision || JSON.stringify(next)!==JSON.stringify(snapshot) || total!==page.total;
    snapshot=next;total=page.total;pageSize=page.page_size;lastMailboxReload=Date.now();
    if(!disposed && changed){render();renderedViewRevision=revision;}
    if(!disposed)renderSyncStatus();
    if($<HTMLDialogElement>('preferences').open)renderAccounts();
  }finally{
    loading=false;
    if(revision!==viewRevision)await reload();
  }
}

const phaseLabels:Record<string,string>={checking_mail:'Checking mailbox',importing:'Importing mail',updating:'Updating messages',downloading:'Downloading mail',scanning_folders:'Scanning folders',saving:'Saving mailbox',complete:'Up to date',paused:'Sync paused',error:'Sync interrupted'};
const number=(n:number)=>n.toLocaleString();
function progressText(s:AccountStatus){
 const p=s.progress;
 if(!p)return s.state==='syncing'?'Connecting to mail provider…':s.detail||'';
 if(p.phase==='connecting')return 'Connecting to mail provider…';
 const count=p.total!==null ? `${number(p.completed)} of about ${number(p.total)}` : number(p.completed);
 return `${phaseLabels[p.phase]||'Syncing mail'} · ${count} ${p.phase==='checking_mail'?'messages checked':'messages'}`;
}
function renderSyncStatus(){
 const states=snapshot.status||[];
 const offlineService=states.some(s=>s.service_available===false);
 const paused=states.some(s=>s.sync_paused);
 const active=states.find(s=>s.state==='syncing'&&!s.sync_paused);
 const problem=states.find(s=>['sync_error','offline','reconnect_required'].includes(s.state));
 const current=active||problem||states.find(s=>s.state==='idle')||states[0];
 $('sync-card').hidden=!current;
 if(!current)return;
 $('sync-title').textContent=offlineService?'Mail service unavailable':paused?'Sync paused':active?'Syncing mail':problem?'Sync needs attention':current.state==='idle'?'Sync paused':'Mail is up to date';
 $('sync-detail').textContent=offlineService?'Cached mail is available. Retry to restart the background mail service.':paused?'Background sync is paused. Cached mail stays available.':active?progressText(active):problem?(problem.detail||'Retry sync or open Settings for account details.'):current.progress?progressText(current):'Background sync is enabled.';
 const cached=states.reduce((n,s)=>n+(s.cached_count||0),0);
 $('sync-cached').textContent=`${number(cached)} cached messages`;
 const meter=$<HTMLProgressElement>('sync-meter');meter.hidden=!active;
 if(active?.progress?.total && active.progress.phase!=='connecting' && active.progress.total>=active.progress.completed){meter.max=active.progress.total;meter.value=active.progress.completed;}else meter.removeAttribute('value');
 $('pause-sync').hidden=offlineService||!active||busy;$('retry-sync').hidden=!offlineService&&!paused&&!problem&&!states.some(s=>s.state==='idle');$('retry-sync').textContent=paused&&!offlineService?'Resume':'Retry';$<HTMLButtonElement>('retry-sync').disabled=busy;
 $('connection-dot').dataset.state=offlineService?'warning':paused?'offline':problem?'warning':active?'syncing':current.state==='connected'?'connected':'offline';
 if(Date.now()>statusUntil)$('status').textContent=offlineService?'Mail service unavailable · Cached mail is available':paused?`${number(cached)} messages cached · Background sync paused`:active?progressText(active):problem?`Mail sync: ${problem.detail}`:`${number(cached)} messages cached · ${current.state==='idle'?'Sync paused':'Background sync enabled'}`;
}
async function pollSync(){
 if(polling||disposed)return;polling=true;
 try{
   const previous=(snapshot.status||[]).reduce((n,s)=>n+(s.cached_count||0),0);
   const states=await api('sync_status');if(disposed)return;
   if(signInActive&&!signInExpiresAt&&!signInBrowserUrl&&!signInExpired&&!signInCancelling){const prompt=await api('account_authorization');if(prompt)showSignInCode(prompt);}
   snapshot.status=states;renderSyncStatus();renderSearchStatus();
   if($<HTMLDialogElement>('preferences').open)renderAccounts();
   const cached=states.reduce((n,s)=>n+(s.cached_count||0),0);
   if(cached!==previous && !loading && Date.now()-lastMailboxReload>10000)await reload();
 }finally{polling=false;}
}
async function loadImages(){
 const m=pendingImageMessage;if(!m||imagesLoading)return;
 $<HTMLDialogElement>('remote-images-confirm').close();imagesLoading=true;renderReader(true);status('Loading remote images for this message…');
 try{
   const loaded=await api('remote_images',{account:m.account,id:m.id});
   const images=remoteImages.get(messageKey(m))||new Map<number,string>();
   for(const item of loaded)if(item.data_url)images.set(item.index,item.data_url);
   remoteImages.set(messageKey(m),images);
   const failures=loaded.filter(i=>i.error).length;
   status(`${loaded.length-failures} images loaded${failures?`; ${failures} stayed blocked or could not be downloaded`:''}.`);
 }finally{imagesLoading=false;if(same(selected,m))renderReader(true);else{const control=$('reader').querySelector<HTMLButtonElement>('.load-images');if(control){control.disabled=false;control.textContent='Load images';}}}
}

function renderSearchStatus(){
 const pending=Math.max(0,...(snapshot.status||[]).map(s=>s.search_pending||0));
 $('search-status').textContent=pending?`Preparing search: ${number(pending)} remaining${$<HTMLInputElement>('search').value?' · results may be incomplete':''}`:'';
}
function renderSelection(){
 $('bulk-actions').hidden=!checked.size&&!lastBulkResult;
 $('selection-count').textContent=`${checked.size} selected`;
 $('bulk-result').textContent=lastBulkResult;
 const page=$<HTMLInputElement>('select-page');
 const visible=isDrafts()?[]:snapshot.messages.filter(m=>!m.pending_move);
 const count=visible.filter(m=>checked.has(messageKey(m))).length;
 page.checked=visible.length>0&&count===visible.length;page.indeterminate=count>0&&count<visible.length;page.disabled=busy||!visible.length;
 for(const id of ['bulk-read','bulk-unread','bulk-star','bulk-unstar','bulk-archive','bulk-trash'])$<HTMLButtonElement>(id).disabled=busy||!checked.size;
}
async function bulkAction(action:Action){
 if(!checked.size||busy)return;
 const targets=[...checked.values()];lastBulkResult='';
 await operation(async()=>{
   const result=await api('bulk_modify',{targets,action});
   for(const target of result.updated)checked.delete(JSON.stringify([target.account,target.id]));
   if(action==='Archive'||action==='Trash'){
     const removed=new Set(result.updated.map(t=>JSON.stringify([t.account,t.id])));
     for(let i=tabs.length-1;i>=0;i--)if(removed.has(messageKey(tabs[i]))){remoteImages.delete(messageKey(tabs[i]));tabs.splice(i,1);}
     if(selected&&removed.has(messageKey(selected)))selected=tabs.at(-1)||null;
   }else if(selected&&result.updated.some(t=>selected?.account===t.account&&selected?.id===t.id)){selected=await api('cached_message',{account:selected.account,id:selected.id});const tab=tabs.findIndex(t=>same(t,selected));if(tab>=0)tabs[tab]=selected;}
   lastBulkResult=`${result.updated.length} updated${result.failed.length?`; ${result.failed.length} not confirmed`:''}${result.not_attempted.length?`; ${result.not_attempted.length} not attempted`:''}${result.cancelled?' · Cancelled':''}.`;
   const details=[...result.failed.map(f=>`${f.target.account}: ${f.detail}`),...result.warnings];
   if(details.length)lastBulkResult+=' '+details.join(' ');
   status(lastBulkResult);renderSelection();
 });
}
function confirmRemoval(account:string){
 if(busy)return;removingAccount=account;$('remove-account-address').textContent=account;$<HTMLDialogElement>('remove-account-confirm').showModal();
}
async function removeAccount(){
 const account=removingAccount;if(!account||busy)return;await flush();
 await operation(async()=>{
   const result=await api('remove_account',{account});
   for(let i=tabs.length-1;i>=0;i--)if(tabs[i].account===account){remoteImages.delete(messageKey(tabs[i]));tabs.splice(i,1);}
   if(selected?.account===account)selected=tabs.at(-1)||null;
   for(const [key,target] of checked)if(target.account===account)checked.delete(key);
   if(folder===account||folderScope()?.[0]===account){folder='All Inboxes';offset=0;viewRevision++;}
   removingAccount=null;$<HTMLDialogElement>('remove-account-confirm').close();
   return result;
 });
}

function readDraft() {
  if(!draft)return;
  for (const key of ['from','to','cc','subject'] as const) draft[key]=$<HTMLInputElement>(key).value;
  draft.body=plainText($('body'));draft.html_body=cleanHtml($('body').innerHTML);
}
async function flush() {
  await senderChange;
  clearTimeout(saveTimer);
  if(draft && dirty) {
    readDraft(); const record=structuredClone(draft);dirty=false;
    saving=saving.catch(()=>{}).then(()=>api('save_draft',{draft:record})).then(()=>{$('save-status').textContent='Saved locally';}).catch(e=>{dirty=true;$('save-status').textContent='Save failed';throw e;});
  }
  await saving;
}
async function openDraft(record:DraftRecord) {
  draft=record.draft;draftFiles=record.files;dirty=false;
  $('from').replaceChildren(...snapshot.accounts.map(a=>{const o=el('option',a);o.value=a;return o;}));
  if(!snapshot.accounts.includes(draft.from)){const placeholder=el('option','Choose a connected sender');placeholder.value='';$('from').prepend(placeholder);}
  for(const key of ['from','to','cc','subject'] as const)$<HTMLInputElement>(key).value=key==='from'&&!snapshot.accounts.includes(draft.from)?'':draft[key];
  $('body').innerHTML=cleanHtml(draft.html_body??textHtml(draft.body));
  $('save-status').textContent='Saved locally';renderDraftFiles();
  if(!$<HTMLDialogElement>('composer').open)$<HTMLDialogElement>('composer').showModal();
}
function renderDraftFiles() {
  $('draft-files').replaceChildren(...draftFiles.map((f,index)=>{
    const row=el('div',undefined,'attachment');row.append(el('span',`${f.name} · ${Math.ceil(f.size/1024)} KiB`),button('Remove',async()=>{await flush();await openDraft(await api('remove_attachment',{id:draft!.id,index}));}));return row;
  }));
  $('forward-files').replaceChildren();
  if(draft?.forward?.attachments.length) {
    $('forward-files').append(el('p','Choose original attachments to include:','muted'));
    draft!.forward!.attachments.forEach((f,index)=>{const l=el('label',undefined,'check'),c=el('input');c.type='checkbox';c.value=String(index);l.append(c,document.createTextNode(f.name));$('forward-files').append(l);});
    $('forward-files').append(button('Include selected attachments',async()=>{
      const selected=[...$('forward-files').querySelectorAll<HTMLInputElement>('input:checked')].map(c=>Number(c.value));
      await flush(); await operation(async()=>openDraft(await api('forward_attachments',{id:draft!.id,selected})));
    }));
  }
}
async function compose(kind='new') {
  await flush();
  await operation(async()=>{
    const record=await api('new_draft',{kind,account:selected?.account||'',id:selected?.id||''});
    await openDraft(record);
    if(!draft!.signature_initialized){await applySignature(false);draft!.signature_initialized=true;draftChanged();await flush();}
  });
}
function previewLink(url:string) {pendingLink=url;$('link-url').textContent=url;$<HTMLDialogElement>('link-preview').showModal();}
function providerFields() {
  const p=$<HTMLInputElement>('provider').value;
  $('google-fields').hidden=p!=='Gmail';$('profile-fields').hidden=p==='Gmail';
  $('microsoft-fields').hidden=p!=='Microsoft365';$('server-fields').hidden=p!=='Manual';
}
function renderAccounts() {
  if(document.activeElement instanceof HTMLSelectElement && $('account-settings').contains(document.activeElement))return;
  const rows=snapshot.accounts.map(a=>{
    const row=el('div',undefined,'attachment');row.append(el('span',a),button('Reconnect',()=>runAccountConnection(()=>api('reconnect',{email:a}),snapshot.status?.find(s=>s.email===a)?.provider==='outlook')),button('Edit',()=>operation(async()=>{
      const profile=await api('account_profile',{email:a});
      if(!profile)throw new Error('No saved server configuration. Use Reconnect.');
      const mapping={email:'account-email',client_id:'client-id',smtp_host:'smtp-host',smtp_port:'smtp-port',smtp_security:'smtp-security',microsoft_sign_in:'microsoft-sign-in'};
      for(const key of ['provider','email','client_id','tenant','username','host','port','security','smtp_host','smtp_port','smtp_security','microsoft_sign_in'])$<HTMLInputElement>(mapping[key as keyof typeof mapping]||key).value=String(profile[key]);
      $<HTMLInputElement>('copy-sent').checked=Boolean(profile.smtp_copy_to_sent);providerFields();
    })));
    row.append(button('Signature',()=>editSignature(a)));
    row.append(button('Remove',()=>confirmRemoval(a),'danger-text'));
    const accountStatus=snapshot.status?.find(s=>s.email===a);
    const historyLabel=el('label','Keep mail available offline');
    const history=el('select');history.setAttribute('aria-label',`Sync history for ${a}`);
    for(const [value,label] of [['one_month','1 month'],['three_months','3 months'],['six_months','6 months'],['one_year','1 year'],['all','All mail']]){
      const option=el('option',label);option.value=value;history.append(option);
    }
    history.value=accountStatus?.sync_history||'one_year';
    history.onchange=()=>operation(()=>api('set_sync_history',{account:a,history:history.value}));
    historyLabel.append(history);row.append(historyLabel);
    if(history.value!=='all')row.append(button('Load older mail',()=>operation(()=>{
      const next:Record<string,SyncHistory>={one_month:'three_months',three_months:'six_months',six_months:'one_year',one_year:'all'};
      return api('set_sync_history',{account:a,history:next[history.value]});
    })));
    if(accountStatus){
      const labels:Record<string,string>={syncing:'Syncing mail…',idle:'Sync paused',connected:'Connected',offline:'Offline',sync_error:'Last sync failed',reconnect_required:'Sign-in required'};
      row.append(el('span',`${accountStatus.service_available===false?'Mail service unavailable':labels[accountStatus.state]||accountStatus.state}${accountStatus.detail?': '+accountStatus.detail:''}`,'muted'));
      if(accountStatus.progress || accountStatus.cached_count!==undefined)row.append(el('span',`${number(accountStatus.cached_count||0)} cached · ${progressText(accountStatus)}`,'muted'));
      if(['sync_error','offline'].includes(accountStatus.state))row.append(button('Retry sync',()=>operation(()=>api('sync'))));
    }
    return row;
  });
  $('account-settings').replaceChildren(...rows);$<HTMLInputElement>('notifications').checked=!!snapshot.notifications;
}
$('older-mail').onclick=openSettings;
$('select-page').addEventListener('change',()=>{
 const page=$<HTMLInputElement>('select-page');const visible=isDrafts()?[]:snapshot.messages.filter(m=>!m.pending_move);
 if(page.checked){const fresh=visible.filter(m=>!checked.has(messageKey(m)));if(checked.size+fresh.length>100){status('Select up to 100 messages at a time.');renderSelection();return;}for(const m of visible)checked.set(messageKey(m),{account:m.account,id:m.id});}
 else for(const m of visible)checked.delete(messageKey(m));render();
});
$('clear-selection').onclick=()=>{checked.clear();lastBulkResult='';render();};
for(const [id,action] of [['bulk-read',{Read:true}],['bulk-unread',{Read:false}],['bulk-star',{Star:true}],['bulk-unstar',{Star:false}],['bulk-archive','Archive']] as [string,Action][])$(id).onclick=()=>safely(()=>bulkAction(action));
$('bulk-trash').onclick=()=>{if(!checked.size||busy)return;$('bulk-trash-summary').textContent=`${checked.size} selected messages will be moved to Trash.`;$<HTMLDialogElement>('bulk-trash-confirm').showModal();};
$('confirm-bulk-trash').onclick=()=>safely(async()=>{$<HTMLDialogElement>('bulk-trash-confirm').close();await bulkAction('Trash');});
$('cancel-bulk-trash').onclick=()=>$<HTMLDialogElement>('bulk-trash-confirm').close();
$('confirm-remove-account').onclick=()=>safely(removeAccount);
$('cancel-remove-account').onclick=()=>{removingAccount=null;$<HTMLDialogElement>('remove-account-confirm').close();};
let searchTimer:number|undefined;
$('search').addEventListener('input',()=>{offset=0;viewRevision++;clearTimeout(searchTimer);searchTimer=setTimeout(()=>safely(reload),300);});
$('previous-page').onclick=()=>safely(async()=>{offset=Math.max(0,offset-pageSize);viewRevision++;await reload();});
$('next-page').onclick=()=>safely(async()=>{offset+=pageSize;viewRevision++;await reload();});
$('compose').onclick=()=>safely(()=>compose());
$('refresh').onclick=()=>safely(()=>operation(()=>api('sync')));
$('retry-sync').onclick=$('refresh').onclick;
$('pause-sync').onclick=()=>safely(async()=>{if(!busy){status(await api('pause_sync'));await pollSync();}});
$('confirm-images').onclick=()=>safely(loadImages);
$('cancel-images').onclick=()=>$<HTMLDialogElement>('remote-images-confirm').close();
$('cancel').onclick=()=>safely(()=>invoke?.('cancel_operation'));
$('settings').onclick=openSettings;
$('add-account').onclick=openSettings;
for(const [id,target] of [['filter-all','All Inboxes'],['filter-unread','Unread'],['filter-starred','Starred']])$(id).onclick=()=>safely(()=>changeFolder(target));
for(const [id,kind] of [['reply','reply'],['reply-all','reply_all'],['forward','forward']])$(id).onclick=()=>safely(()=>compose(kind));
$('archive').onclick=()=>safely(()=>modify('Archive'));
$('trash').onclick=()=>safely(()=>modify('Trash'));
$('mark-read').onclick=()=>safely(()=>selected && modify({Read:selected.unread}));
$('close-settings').onclick=()=>$<HTMLDialogElement>('preferences').close();
$('provider').onchange=providerFields;
$('notifications').onchange=()=>safely(()=>api('notifications',{enabled:$<HTMLInputElement>('notifications').checked}));
function draftChanged(){
  dirty=true;$('save-status').textContent='Saving…';clearTimeout(saveTimer);saveTimer=setTimeout(()=>safely(flush),500);
}
for(const key of ['to','cc','subject'])$(key).addEventListener('input',draftChanged);
setupEditor($('body'),$('compose-toolbar'),draftChanged);
setupEditor($('signature-editor'),$('signature-toolbar'),()=>{$('signature-status').textContent='';});
let senderChange:Promise<void>=Promise.resolve();
$('from').addEventListener('change',()=>{
  const account=$<HTMLSelectElement>('from').value;
  senderChange=senderChange.catch(()=>{}).then(async()=>{
    await applySignature(true,account);draftChanged();
  });
  safely(()=>senderChange);
});
async function applySignature(replace:boolean,account=$<HTMLSelectElement>('from').value){
  if(!draft||!account)return;
  const currentDraft=draft;
  const signature=await api('signature',{account});
  if(draft!==currentDraft)return;
  const editor=$('body');
  if(replace)editor.querySelectorAll('[data-focal-signature]').forEach(node=>node.remove());
  const reply=!!draft.reply_account||!!draft.forward;
  if(signature && (reply?signature.replies_forwards:signature.new_messages) && signature.html){
    const block=el('div');block.dataset.focalSignature='true';block.innerHTML=cleanHtml(signature.html);
    block.prepend(el('br'));block.append(el('br'));
    if(reply)editor.prepend(block);else editor.append(block);
  }
  draft.signature_initialized=true;
}
async function editSignature(account:string){
 const signature=await api('signature',{account});signatureAccount=account;
 $('signature-account').textContent=account;
 $('signature-editor').innerHTML=cleanHtml(signature.html);
 $<HTMLInputElement>('signature-new').checked=signature.new_messages;
 $<HTMLInputElement>('signature-replies').checked=signature.replies_forwards;
 $('signature-status').textContent='';$<HTMLDialogElement>('signature-settings').showModal();
}
$('close-signature').onclick=()=>$<HTMLDialogElement>('signature-settings').close();
$('save-signature').onclick=()=>safely(async()=>{
 try{
  await api('set_signature',{account:signatureAccount,signature:{html:cleanHtml($('signature-editor').innerHTML),new_messages:$<HTMLInputElement>('signature-new').checked,replies_forwards:$<HTMLInputElement>('signature-replies').checked}});
  $<HTMLDialogElement>('signature-settings').close();
 }catch(error){$('signature-status').textContent=String(error);}
});
async function closeComposer(){await flush();$<HTMLDialogElement>('composer').close();await reload();}
$('close-compose').onclick=()=>safely(closeComposer);
$('composer').addEventListener('cancel',e=>{e.preventDefault();if(!busy)safely(closeComposer);});
$('draft-form').onsubmit=e=>{e.preventDefault();safely(async()=>{
  await senderChange;await flush(); await operation(async()=>{const result=await api('send',{id:draft!.id});draft=null;dirty=false;$<HTMLDialogElement>('composer').close();return result;});
});};
$('discard').onclick=()=>safely(async()=>{await flush();await api('delete_draft',{id:draft!.id});draft=null;dirty=false;$<HTMLDialogElement>('composer').close();await reload();});
$('attachment').onchange=()=>safely(async()=>{
  const file=$<HTMLInputElement>('attachment').files?.[0];if(!file)return;
  try {
    if(file.size+draftFiles.reduce((n,f)=>n+f.size,0)>20*1024*1024)throw new Error('Attachments may total at most 20 MiB.');
    await flush();const data=Array.from(new Uint8Array(await file.arrayBuffer()));
    await operation(async()=>openDraft(await api('attach',{id:draft!.id,name:file.name,data})));
  }finally{$<HTMLInputElement>('attachment').value='';}
});
$('account-form').onsubmit=e=>{e.preventDefault();safely(()=>runAccountConnection(async()=>{
  if($<HTMLInputElement>('provider').value==='Gmail')return api('connect',{path:$<HTMLInputElement>('oauth-path').value});
  const profile={provider:$<HTMLInputElement>('provider').value,email:$<HTMLInputElement>('account-email').value,incoming:'Imap',authentication:$<HTMLInputElement>('provider').value==='Manual'?'Password':'OAuth',host:$<HTMLInputElement>('host').value,port:Number($<HTMLInputElement>('port').value),security:$<HTMLInputElement>('security').value,smtp_host:$<HTMLInputElement>('smtp-host').value,smtp_port:Number($<HTMLInputElement>('smtp-port').value),smtp_security:$<HTMLInputElement>('smtp-security').value,smtp_copy_to_sent:$<HTMLInputElement>('copy-sent').checked,username:$<HTMLInputElement>('username').value,client_id:$<HTMLInputElement>('client-id').value,tenant:$<HTMLInputElement>('tenant').value,microsoft_sign_in:$<HTMLSelectElement>('microsoft-sign-in').value};
  const password=$<HTMLInputElement>('password').value;$<HTMLInputElement>('password').value='';return api('connect_profile',{profile,password});
},$<HTMLInputElement>('provider').value==='Microsoft365'));};
$('copy-signin-code').onclick=()=>safely(async()=>{const code=$<HTMLInputElement>('account-signin-code');if(!code.value)return;try{await navigator.clipboard.writeText(code.value);$('account-signin-status').textContent='Code copied. Open Microsoft sign-in and enter it.';}catch{code.focus();code.select();$('account-signin-status').textContent='Code selected. Press Ctrl+C to copy it.';}});
$('open-microsoft-signin').onclick=()=>safely(async()=>{await api('open_link',{url:'https://microsoft.com/devicelogin'});$('account-signin-status').textContent='Complete sign-in in your browser. This dialog will stay open.';});
$('open-microsoft-browser').onclick=()=>safely(async()=>{if(signInBrowserUrl)await api('open_link',{url:signInBrowserUrl});});
$('cancel-account-signin').onclick=()=>safely(cancelAccountSignIn);
$('close-account-signin').onclick=()=>{clearSignInCode();$<HTMLDialogElement>('account-signin').close();};
$('account-signin').addEventListener('cancel',event=>{if(signInActive){event.preventDefault();safely(cancelAccountSignIn);}else clearSignInCode();});
$('open-link').onclick=()=>safely(async()=>{await api('open_link',{url:pendingLink});$<HTMLDialogElement>('link-preview').close();});
$('close-link').onclick=()=>$<HTMLDialogElement>('link-preview').close();
const beforeUnload=(e:BeforeUnloadEvent)=>{if(dirty){e.preventDefault();e.returnValue='';safely(flush);}};
window.addEventListener('beforeunload',beforeUnload);
const shortcuts=(e:KeyboardEvent)=>{if((e.ctrlKey||e.metaKey)&&e.key.toLowerCase()==='k'){e.preventDefault();$('search').focus();}if((e.ctrlKey||e.metaKey)&&e.key.toLowerCase()==='n'&&!$<HTMLDialogElement>('composer').open){e.preventDefault();safely(()=>compose());}};
window.addEventListener('keydown',shortcuts);
async function start() {
  if(window.__TAURI__)await safely(ensureProgressListeners);
  poll=window.setInterval(()=>safely(pollSync),2000);
  interval=window.setInterval(()=>{if(!loading&&!disposed)safely(reload);},30000);
  await reload();
  const problem=snapshot.status?.find(s=>['sync_error','offline','reconnect_required'].includes(s.state));
  status(problem ? `Mail sync: ${problem.detail || problem.state}. Cached mail remains available.` : snapshot.accounts.length?'Mail service syncs in the background.':'Add an account in Settings to get started.');
  if(snapshot.active && snapshot.drafts.some(d=>d.id===snapshot.active)) {
    const record=await api('load_draft',{id:snapshot.active});draft=record.draft;draftFiles=record.files;
  }
}
safely(start);

return ()=>{disposed=true;clearTimeout(saveTimer);clearTimeout(searchTimer);clearInterval(interval);clearInterval(poll);clearSignInCode();disposeFrame?.();unlisten?.();window.removeEventListener('beforeunload',beforeUnload);window.removeEventListener('keydown',shortcuts);};
}
