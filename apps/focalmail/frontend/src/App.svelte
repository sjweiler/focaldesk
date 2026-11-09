<script lang="ts">
 import {onMount} from 'svelte';
 import Icon from './Icon.svelte';
 import {mountMailClient} from './mail';
 function stored(key:string,fallback:number,min:number,max:number){
   try {const value=Number(localStorage.getItem(key));return value ? Math.max(min,Math.min(max,value)) : fallback;}catch{return fallback;}
 }
 let sidebar=$state(stored('focalmail.sidebar',228,180,320));
 let list=$state(stored('focalmail.list',360,280,560));
 let stopDrag=()=>{};
 function setWidth(which:'sidebar'|'list',value:number){
   const min=which==='sidebar'?180:280;
   const max=Math.min(which==='sidebar'?320:560,window.innerWidth-(which==='sidebar'?list:sidebar)-390);
   const width=Math.round(Math.max(min,Math.min(max,value)));
   if(which==='sidebar')sidebar=width;else list=width;
   try{localStorage.setItem(`focalmail.${which}`,String(width));}catch{/* Session layout still works without storage. */}
 }
 function resize(event:PointerEvent,which:'sidebar'|'list'){
   if(event.button!==0)return;
   event.preventDefault();stopDrag();
   const start=event.clientX,original=which==='sidebar'?sidebar:list;
   const move=(e:PointerEvent)=>setWidth(which,original+e.clientX-start);
   const end=()=>{window.removeEventListener('pointermove',move);window.removeEventListener('pointerup',end);document.body.classList.remove('resizing');};
   stopDrag=end;document.body.classList.add('resizing');window.addEventListener('pointermove',move);window.addEventListener('pointerup',end,{once:true});
 }
 function resizeKey(event:KeyboardEvent,which:'sidebar'|'list'){
   if(!['ArrowLeft','ArrowRight','Home'].includes(event.key))return;
   event.preventDefault();
   setWidth(which,event.key==='Home'?(which==='sidebar'?228:360):(which==='sidebar'?sidebar:list)+(event.key==='ArrowRight'?16:-16));
 }
 onMount(()=>{
   const fit=()=>{setWidth('list',list);setWidth('sidebar',sidebar);};fit();
   window.addEventListener('resize',fit);
   const dispose=mountMailClient();return()=>{stopDrag();window.removeEventListener('resize',fit);dispose();};
 });
</script>
<div class="app-shell" style:--sidebar-width={`${sidebar}px`} style:--list-width={`${list}px`}>
 <header class="titlebar"><div class="brand"><span class="app-icon"><Icon name="mail"/></span><span>FocalDesk <strong>Mail</strong></span></div><span class="workspace-caption">Your mail. Your workspace.</span><span class="local-badge"><Icon name="lock"/> Local cache</span></header>
 <div class="commandbar">
  <div class="compose-area"><button id="compose" class="primary"><Icon name="edit"/><span>New Mail</span><kbd>Ctrl N</kbd></button></div>
  <div class="search-area"><Icon name="search"/><input id="search" type="search" maxlength="512" aria-label="Search cached mail" placeholder="Search messages…"><kbd>Ctrl K</kbd></div>
  <div class="command-actions" aria-label="Message actions">
   <button id="reply" class="tool" disabled><Icon name="reply"/><span>Reply</span></button>
   <button id="reply-all" class="tool" disabled><Icon name="replyAll"/><span>Reply all</span></button>
   <button id="forward" class="tool" disabled><Icon name="forward"/><span>Forward</span></button>
   <span class="tool-divider"></span>
   <button id="archive" class="tool" disabled><Icon name="archive"/><span>Archive</span></button>
   <button id="trash" class="tool" disabled><Icon name="trash"/><span>Delete</span></button>
   <button id="mark-read" class="tool" disabled><Icon name="unread"/><span>Mark read</span></button>
   <div class="toolbar-end"><button id="refresh" class="icon-button" aria-label="Refresh mail" title="Refresh mail"><Icon name="refresh"/></button><button id="settings" class="icon-button" aria-label="Settings" title="Settings"><Icon name="settings"/></button></div>
  </div>
 </div>
 <main>
  <nav class="sidebar" aria-label="Mail navigation"><div class="sidebar-navigation">
   <section class="nav-section"><div class="section-label"><h2>Accounts</h2><button id="add-account" class="small-icon" aria-label="Add account"><Icon name="plus"/></button></div><div id="accounts"></div></section>
   <section class="nav-section"><div class="section-label"><h2>Folders</h2><Icon name="chevron"/></div><div id="folders"></div></section>
   <section class="nav-section"><div class="section-label"><h2>Smart views</h2><Icon name="chevron"/></div><div id="smart-folders"></div></section>
   </div><div id="sync-card" class="sync-card" hidden><div class="sync-heading"><strong id="sync-title">Mail sync</strong><button id="pause-sync" class="small-sync-button" hidden>Pause</button><button id="retry-sync" class="small-sync-button" hidden>Retry</button></div><p id="sync-detail"></p><progress id="sync-meter" aria-label="Mail sync progress" hidden></progress><span id="sync-cached"></span></div>
   <div class="sidebar-note"><span class="note-icon"><Icon name="shield"/></span><div><strong>Private by default</strong><p>Remote images stay blocked.</p></div></div>
  </nav>
  <!-- Focusable separators follow the WAI-ARIA window splitter pattern. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div class="splitter" role="separator" aria-label="Resize account sidebar" aria-orientation="vertical" aria-valuenow={sidebar} aria-valuemin="180" aria-valuemax="320" tabindex="0" onpointerdown={e=>resize(e,'sidebar')} onkeydown={e=>resizeKey(e,'sidebar')}></div>
  <section class="mail-list" aria-label="Messages">
   <div class="list-heading"><div><span class="eyebrow">MAILBOX</span><h1 id="folder-title">All Inboxes</h1><span class="sort-order">Newest first · Local time</span></div><span id="count">0</span></div>
   <div class="list-filters" aria-label="Inbox filters"><button id="filter-all" class="active">All</button><button id="filter-unread">Unread</button><button id="filter-starred">Starred</button></div>
   <div class="selection-heading"><label class="check"><input id="select-page" type="checkbox" aria-label="Select all messages on this page">Select page</label><span id="search-status" role="status"></span></div>
   <div id="bulk-actions" class="bulk-actions" hidden><div><strong id="selection-count"></strong><button id="clear-selection">Clear</button></div><div class="bulk-buttons"><button id="bulk-read">Read</button><button id="bulk-unread">Unread</button><button id="bulk-star">Star</button><button id="bulk-unstar">Unstar</button><button id="bulk-archive">Archive</button><button id="bulk-trash">Trash</button></div><p id="bulk-result" role="status"></p></div>
   <div id="messages" aria-live="polite"></div>
   <button id="older-mail" class="small-sync-button">Need older mail? Adjust sync history…</button>
   <div id="pagination" class="pagination" hidden><button id="previous-page">Previous</button><span>100 per page</span><button id="next-page">Next</button></div>
  </section>
  <!-- Focusable separators follow the WAI-ARIA window splitter pattern. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div class="splitter" role="separator" aria-label="Resize message list" aria-orientation="vertical" aria-valuenow={list} aria-valuemin="280" aria-valuemax="560" tabindex="0" onpointerdown={e=>resize(e,'list')} onkeydown={e=>resizeKey(e,'list')}></div>
  <section class="reading-pane" aria-label="Message reader"><div id="message-tabs" class="message-tabs" role="tablist" aria-label="Open messages"></div><div id="reader" role="tabpanel" tabindex="0"><div class="empty">Select a message to read</div></div></section>
 </main>
 <footer><span id="connection-dot" class="connection-dot"></span><span id="status" role="status">Opening your mailbox…</span><button id="cancel" hidden>Cancel operation</button><span class="footer-privacy"><Icon name="shield"/>Remote content protected</span></footer>
</div>
<dialog id="composer"><form id="draft-form"><div class="dialog-heading"><h1>New message</h1><button type="button" id="close-compose" aria-label="Close composer">✕</button></div><label>From<select id="from" required></select></label><label>To<input id="to" required></label><label>Cc<input id="cc"></label><label>Subject<input id="subject"></label><textarea id="body" aria-label="Message body" placeholder="Write your message…"></textarea><div id="draft-files"></div><div id="forward-files"></div><div class="actions"><button type="submit" class="primary" id="send">Send message</button><label class="file-button">Attach file<input id="attachment" type="file" hidden></label><button type="button" id="discard">Discard draft</button><span id="save-status" class="muted"></span></div></form></dialog>
<dialog id="preferences"><div class="dialog-heading"><h1>Settings</h1><button id="close-settings" aria-label="Close settings">✕</button></div><h2>Connected accounts</h2><p class="muted">Sync the last year by default. Choose a longer history or Load older mail to fetch earlier messages. Reducing history removes cached copies only; mail on the server stays unchanged. Attachments load when needed. For IMAP, message bodies load when opened.</p><div id="account-settings"></div><label class="check"><input id="notifications" type="checkbox"> Notify about new unread mail</label><hr><h2>Add or edit an account</h2><form id="account-form"><label>Provider<select id="provider"><option value="Gmail">Google / Gmail</option><option value="Microsoft365">Microsoft 365 / Outlook</option><option value="Manual">Other mail server</option></select></label><div id="google-fields"><label>Application OAuth JSON path<input id="oauth-path" placeholder="/absolute/path/client_secret.json"></label><p class="muted">Use a Google Desktop app OAuth client. Sign-in opens in your browser.</p></div><div id="profile-fields" hidden><label>Email<input id="account-email" type="email"></label><div id="microsoft-fields"><label>Application client ID<input id="client-id"></label><label>Tenant<input id="tenant" value="common"></label><label>Sign-in method<select id="microsoft-sign-in"><option value="Browser">Browser sign-in (recommended)</option><option value="DeviceCode">Device code</option></select></label><p class="muted">Browser sign-in supports Microsoft MFA. Configure http://localhost as a Mobile and desktop application redirect URI in the FocalDesk app registration. Microsoft Security Defaults can block device-code sign-in.</p></div><div id="server-fields" hidden><label>Username<input id="username" placeholder="Defaults to email"></label><div class="columns"><label>IMAP host<input id="host"></label><label>Port<input id="port" type="number" min="1" max="65535" value="993"></label><label>Security<select id="security"><option value="Tls">SSL/TLS</option><option value="StartTls">STARTTLS</option></select></label></div><div class="columns"><label>SMTP host<input id="smtp-host"></label><label>Port<input id="smtp-port" type="number" min="1" max="65535" value="587"></label><label>Security<select id="smtp-security"><option value="StartTls">STARTTLS</option><option value="Tls">SSL/TLS</option></select></label></div><label>Password / app password<input id="password" type="password" autocomplete="off"></label><label class="check"><input id="copy-sent" type="checkbox" checked>Save a copy in the server Sent folder</label></div></div><button type="submit" class="primary">Connect account</button></form></dialog>
<dialog id="account-signin" aria-labelledby="account-signin-title"><h1 id="account-signin-title">Connect Microsoft 365</h1><p id="account-signin-status" role="status" aria-live="polite"></p><div id="account-signin-browser-area" hidden><p>Complete Microsoft sign-in and MFA in your browser. FocalMail will receive the result automatically.</p><button id="open-microsoft-browser" class="primary">Open Microsoft sign-in</button></div><div id="account-signin-code-area" hidden><p>Copy this code, then open Microsoft sign-in and enter it.</p><label>Sign-in code<input id="account-signin-code" readonly autocomplete="off" spellcheck="false"></label><p id="account-signin-expiry" class="muted"></p><div class="actions"><button id="copy-signin-code">Copy code</button><button id="open-microsoft-signin" class="primary">Open Microsoft sign-in</button></div><p class="muted">https://microsoft.com/devicelogin</p></div><div class="actions"><button id="cancel-account-signin">Cancel sign-in</button><button id="close-account-signin" hidden>Close</button></div></dialog>
<dialog id="link-preview"><h1>Open link in browser?</h1><p id="link-url"></p><div class="actions"><button id="open-link" class="primary">Open in browser</button><button id="close-link">Cancel</button></div></dialog>


<dialog id="remote-images-confirm"><h1>Load remote images?</h1><p>Loading images contacts their servers and may tell the sender that you opened this message. Images are loaded for this message only.</p><p class="muted">Up to 20 public HTTPS images, 2 MiB each. Local network URLs and active image formats stay blocked.</p><div class="actions"><button id="confirm-images" class="primary">Load images</button><button id="cancel-images">Keep blocked</button></div></dialog>

<dialog id="bulk-trash-confirm"><h1>Move selected messages to Trash?</h1><p id="bulk-trash-summary"></p><p class="muted">This moves mail to each account’s server Trash folder. It does not permanently delete it.</p><div class="actions"><button id="confirm-bulk-trash" class="danger">Move to Trash</button><button id="cancel-bulk-trash">Cancel</button></div></dialog>
<dialog id="remove-account-confirm"><h1>Remove account from this device?</h1><p id="remove-account-address"></p><p>This clears the account’s sign-in, saved settings, and cached mail from this computer. Mail on the server is unchanged.</p><p class="muted">Local drafts and their attachments are retained. Choose a connected sender before sending those drafts.</p><div class="actions"><button id="confirm-remove-account" class="danger">Remove account</button><button id="cancel-remove-account">Keep account</button></div></dialog>
