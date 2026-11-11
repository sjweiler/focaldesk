// Real browser regression checks with a mock Rust boundary; no mail credentials.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {spawn} = require('node:child_process');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const {createServer} = require('node:http');
const {createHash}=require('node:crypto');

test('mailbox, safe rendering, drafts and send failures in the browser', async t => {
  const profile=await fs.mkdtemp(path.join(os.tmpdir(),'focalmail-web-'));
  const chrome=spawn(process.env.CHROME_BIN || 'google-chrome',['--headless','--no-sandbox','--disable-gpu','--window-size=1380,900','--no-first-run','--no-default-browser-check','--remote-debugging-port=0',`--user-data-dir=${profile}`,'about:blank'],{stdio:'ignore'});
  let launchError;chrome.on('error',e=>{launchError=e;});
  let socket;
  t.after(async()=>{if(socket)socket.close();chrome.kill();await new Promise(r=>chrome.exitCode!==null?r():chrome.once('exit',r));await fs.rm(profile,{recursive:true,force:true,maxRetries:5});});
  let port;
  for(let i=0;i<100;i++){
    if(launchError){t.skip(`Chrome is unavailable: ${launchError.message}`);return;}
    try{port=(await fs.readFile(path.join(profile,'DevToolsActivePort'),'utf8')).split('\n')[0];break;}catch{await new Promise(r=>setTimeout(r,100));}
  }
  assert.ok(port,'Chrome debugger started');
  const tabs=await (await fetch(`http://127.0.0.1:${port}/json`)).json();
  socket=new WebSocket(tabs[0].webSocketDebuggerUrl);
  await new Promise((resolve,reject)=>{socket.onopen=resolve;socket.onerror=reject;});
  let nextId=0;const pending=new Map();
  socket.onmessage=e=>{const message=JSON.parse(e.data);if(message.id){const p=pending.get(message.id);pending.delete(message.id);message.error?p.reject(message.error):p.resolve(message.result);}};
  const cdp=(method,params={},sessionId)=>new Promise((resolve,reject)=>{const id=++nextId;pending.set(id,{resolve,reject});socket.send(JSON.stringify({id,method,params,...(sessionId?{sessionId}:{})}));});
  const evaluate=async expression=>{
    const result=await cdp('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});
    assert.equal(result.exceptionDetails,undefined,JSON.stringify(result.exceptionDetails));return result.result.value;
  };
  await cdp('Page.enable');
  await cdp('Emulation.setTimezoneOverride',{timezoneId:'America/New_York'});
  await cdp('Emulation.setLocaleOverride',{locale:'en-US'});
  await cdp('Page.addScriptToEvaluateOnNewDocument',{source:`
    window.signatures={};window.accounts=['me@example.com','work@example.com'];window.bulkPartial=false;window.calls=[];window.failSend=false;window.saved=[];window.mailboxSize=1;window.resolveSync=null;window.previewMessages=null;
    window.mail={id:'one',account:'me@example.com',sender:'<img src=x onerror=alert(1)>',email:'sender@example.com',subject:'<script>evil()</script>',body:'Offline body',date:'Today',folder:'Inbox',labels:['INBOX','SENT'],unread:true,flagged:false,timestamp:1,html_loaded:true,attachments_loaded:true,attachments:[],html:{blocks:[{spans:[{text:'<img src="https://tracker.invalid">',bold:true},{text:'Read more',link:'https://example.com/path'}]}],blocked_images:1,limited:false}};
    window.statuses=[{email:'me@example.com',provider:'gmail',state:'connected',detail:'',cached_count:1},{email:'work@example.com',provider:'outlook',state:'connected',detail:'',cached_count:0}];
    window.localDraft={id:'draft-one',from:'me@example.com',to:'you@example.com',cc:'',subject:'Hello',body:'Saved body'};
    window.eventHandlers={};window.mockSignInMode='event';window.activeAuthPrompt=null;window.resolveAuth=null;window.rejectAuth=null;
    window.__TAURI__={event:{listen:async(name,callback)=>{eventHandlers[name]=callback;return ()=>delete eventHandlers[name];}},core:{invoke:async(command,args)=>{
      if(command==='cancel_operation'){activeAuthPrompt=null;if(rejectAuth)rejectAuth('Operation cancelled');return;}
      if(command!=='mail_request')return;
      const r=args.request;calls.push(structuredClone(r));
      switch(r.op){
        case 'signature':return structuredClone(signatures[r.account]||{html:'',new_messages:false,replies_forwards:false});
        case 'set_signature':signatures[r.account]=structuredClone(r.signature);return null;
        case 'load_draft':return {draft:structuredClone(saved.at(-1)||localDraft),files:[]};
        case 'attach':return {draft:structuredClone(saved.at(-1)||localDraft),files:[{name:r.name,size:r.data.length}]};
        case 'account_authorization':return activeAuthPrompt;
        case 'connect_profile':
          window.authListenersReady=!!eventHandlers['mail-device-authorization']&&!!eventHandlers['mail-progress'];
          if(mockSignInMode==='error')throw 'Microsoft could not issue a sign-in code. Check the client ID and public client settings.';
          activeAuthPrompt=r.profile.microsoft_sign_in==='Browser'?{user_code:'',expires_in:600,verification_uri:'https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id=test-client&state=test-state&code_challenge=pkce-test&code_challenge_method=S256'}:{user_code:'TEST-CODE',expires_in:900};
          if(mockSignInMode==='event')queueMicrotask(()=>eventHandlers['mail-device-authorization']?.({payload:activeAuthPrompt}));
          return new Promise((resolve,reject)=>{resolveAuth=resolve;rejectAuth=reject;});
        case 'snapshot':return {folders:{'work@example.com':['Projects/Client A','Empty folder']},accounts:accounts,drafts:[],active:null,notifications:true,status:statuses};
        case 'mailbox':if(previewMessages)return {messages:[...previewMessages].sort((a,b)=>b.timestamp-a.timestamp),total:previewMessages.length,page_size:100};return {messages:Array.from({length:Math.max(0,Math.min(100,mailboxSize-r.offset))},(_,i)=>({...mail,id:r.offset+i===0?'one':'page-'+(r.offset+i)})),total:mailboxSize,page_size:100};
        case 'sync_status':return structuredClone(statuses);
        case 'set_sync_history':statuses.find(s=>s.email===r.account).sync_history=r.history;return 'Sync history saved';
        case 'pause_sync':statuses[0].state='idle';statuses[0].sync_paused=true;statuses[0].progress.phase='paused';return 'Background sync paused';
        case 'remote_images':return [{index:0,data_url:'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7',error:null}];
        case 'sync':return new Promise(resolve=>{resolveSync=resolve;});
        case 'cached_message':return structuredClone(previewMessages?.find(m=>m.id===r.id)||{...mail,id:r.id});
        case 'new_draft':return {draft:{...structuredClone(localDraft),reply_account:r.kind==='new'?'':'me@example.com'},files:[]};
        case 'save_draft':saved.push(structuredClone(r.draft));return null;
        case 'send':if(failSend)throw 'Send outcome unknown; check Sent before retrying';return 'Message sent';
        case 'bulk_modify':return {updated:bulkPartial?r.targets.slice(0,1):r.targets,failed:bulkPartial?r.targets.slice(1).map(target=>({target,detail:'Simulated failure'})):[],not_attempted:[],warnings:[],cancelled:false};
        case 'remove_account':accounts=accounts.filter(a=>a!==r.account);statuses=statuses.filter(s=>s.email!==r.account);return 'Account removed';
        case 'open_link':return null;
      }
    }}};
  `});
  const dist=path.resolve(__dirname,'../dist');
  const config=JSON.parse(await fs.readFile(path.resolve(__dirname,'../tauri.conf.json'),'utf8'));
  const frameScript=await fs.readFile(path.resolve(__dirname,'../frontend/src/mail-frame.js'));
  const frameHash='sha256-'+createHash('sha256').update(frameScript).digest('base64');
  assert.ok(config.app.security.csp.includes(`'${frameHash}'`),'CSP allows only the shipped frame bootstrap');
  assert.ok((await fs.readFile(path.resolve(__dirname,'../frontend/src/frame.ts'),'utf8')).includes(frameHash));
  const server=createServer(async(req,res)=>{
    try {
      const pathname=new URL(req.url,'http://localhost').pathname;
      const file=path.join(dist,pathname==='/'?'index.html':pathname);
      if(!file.startsWith(dist+path.sep)){res.writeHead(403);res.end();return;}
      const type=file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html';
      const contents=await fs.readFile(file);
      res.writeHead(200,{'Content-Type':type,'Content-Security-Policy':config.app.security.csp});res.end(contents);
    }catch{res.writeHead(404);res.end();}
  });
  await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(0,'127.0.0.1',resolve);});
  t.after(()=>new Promise(resolve=>{server.closeAllConnections();server.close(resolve);}));
  await cdp('Page.navigate',{url:`http://127.0.0.1:${server.address().port}/`});
  for(let i=0;i<100;i++){
    if(await evaluate(`!!document.querySelector('.message')`))break;
    await new Promise(r=>setTimeout(r,50));
  }
  assert.equal(await evaluate(`document.querySelectorAll('.message').length`),1);
  await evaluate(`document.querySelector('.message').click();new Promise(r=>setTimeout(r,30))`);
  assert.equal(await evaluate(`document.querySelectorAll('#reader img, #reader script, #reader iframe').length`),0,'Sender content is text, never HTML');
  assert.equal(await evaluate(`document.querySelector('#reader .message-body strong').textContent`),'<img src="https://tracker.invalid">');
  assert.equal(await evaluate(`calls.filter(c=>c.op==='modify').length`),0,'Opening mail does not mark it read');
  await evaluate(`document.querySelector('#reader .link').click()`);
  assert.equal(await evaluate(`document.querySelector('#link-preview').open`),true);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='open_link').length`),0,'Link requires explicit browser confirmation');
  await evaluate(`document.querySelector('#close-link').click()`);

  await evaluate(`
    previewMessages=[
      {...mail,id:'older-date',timestamp:1793939128000,date:'Fri, 06 Nov 2026 09:55:28 +0530'},
      {...mail,id:'newest-date',timestamp:1793943883000,date:'Thu, 5 Nov 2026 21:44:43 -0800'},
      {...mail,id:'middle-date',timestamp:1793939211000,date:'Thu, 05 Nov 2026 22:26:51 -0600'}
    ];document.querySelector('#filter-all').click();new Promise(r=>setTimeout(r,50));
  `);
  assert.match(await evaluate(`document.querySelector('.sort-order').textContent`),/Newest first.*Local time/);
  assert.deepEqual(await evaluate(`[...document.querySelectorAll('.message time')].map(t=>t.dateTime)`),['2026-11-06T05:44:43.000Z','2026-11-06T04:26:51.000Z','2026-11-06T04:25:28.000Z']);
  assert.equal(await evaluate(`document.querySelectorAll('.date-divider').length`),2,'Mixed sender time zones are grouped into two actual local calendar days');
  assert.match(await evaluate(`document.querySelector('.message time').textContent`),/12:44/);
  assert.match(await evaluate(`document.querySelector('.message time').title`),/EST/);
  await evaluate(`document.querySelector('.message').click();new Promise(r=>setTimeout(r,30))`);
  assert.match(await evaluate(`document.querySelector('.metadata').textContent`),/12:44/,'Reader and list display the same normalized date');
  await evaluate(`document.querySelector('.message-tab.active .tab-close').click();
    const today=new Date();today.setHours(9,0,0,0);const yesterday=new Date(today);yesterday.setDate(today.getDate()-1);
    previewMessages=[{...mail,id:'today-date',timestamp:today.getTime()},{...mail,id:'yesterday-date',timestamp:yesterday.getTime()},{...mail,id:'unknown-date',timestamp:0,date:'11-04-2026'}];
    document.querySelector('#filter-all').click();new Promise(r=>setTimeout(r,50));
  `);
  assert.deepEqual(await evaluate(`[...document.querySelectorAll('.date-divider')].map(e=>e.textContent)`),['Today','Yesterday','No date']);
  await evaluate(`previewMessages=null;document.querySelector('#filter-all').click();new Promise(r=>setTimeout(r,50))`);

  await evaluate(`document.querySelector('#reply-all').click();new Promise(r=>setTimeout(r,60))`);
  assert.equal(await evaluate(`calls.find(c=>c.op==='new_draft').kind`),'reply_all');
  await evaluate(`document.querySelector('#body').textContent='Changed offline draft';document.querySelector('#body').dispatchEvent(new Event('input'));new Promise(r=>setTimeout(r,650))`);
  assert.equal(await evaluate(`saved.at(-1).body`),'Changed offline draft');
  await evaluate(`failSend=true;document.querySelector('#draft-form').dispatchEvent(new Event('submit',{cancelable:true}));new Promise(r=>setTimeout(r,100))`);
  assert.equal(await evaluate(`document.querySelector('#composer').open`),true,'Failed send retains composer');
  assert.equal(await evaluate(`document.querySelector('#body').innerText`),'Changed offline draft');
  assert.equal(await evaluate(`calls.filter(c=>c.op==='send').length`),1,'No automatic send retries');
  assert.match(await evaluate(`document.querySelector('#status').textContent`),/check Sent/);
  await evaluate(`failSend=false;document.querySelector('#draft-form').dispatchEvent(new Event('submit',{cancelable:true}));new Promise(r=>setTimeout(r,100))`);
  assert.equal(await evaluate(`document.querySelector('#composer').open`),false,'Successful send closes composer');

  // Set formatted signatures independently of provider authentication.
  await evaluate(`document.querySelector('#settings').click();[...document.querySelectorAll('#account-settings button')].find(b=>b.textContent==='Signature').click();new Promise(r=>setTimeout(r,40))`);
  await evaluate(`document.querySelector('#signature-editor').innerHTML='<p><b>Personal Team</b></p><img src="https://tracker.invalid/pixel" onerror="window.badPaste=true">';document.querySelector('#signature-new').checked=true;document.querySelector('#signature-replies').checked=false;document.querySelector('#save-signature').click();new Promise(r=>setTimeout(r,40))`);
  assert.equal(await evaluate(`signatures['me@example.com'].html`),'<p><b>Personal Team</b></p>');
  await evaluate(`document.querySelector('#close-settings').click();signatures['work@example.com']={html:'<p><i>Work Team</i></p>',new_messages:true,replies_forwards:true};document.querySelector('#compose').click();new Promise(r=>setTimeout(r,60))`);
  assert.equal(await evaluate(`document.querySelectorAll('#body [data-focal-signature]').length`),1);
  assert.match(await evaluate(`saved.at(-1).html_body`),/<b>Personal Team<\/b>/);
  // Preserve edited message text while replacing only the inserted signature.
  await evaluate(`document.querySelector('#body').prepend(document.createTextNode('Keep my message'));document.querySelector('#from').value='work@example.com';document.querySelector('#from').dispatchEvent(new Event('change'));new Promise(r=>setTimeout(r,650))`);
  assert.equal(await evaluate(`document.querySelectorAll('#body [data-focal-signature]').length`),1);
  assert.match(await evaluate(`saved.at(-1).body`),/Keep my message/);
  assert.match(await evaluate(`saved.at(-1).html_body`),/<i>Work Team<\/i>/);
  assert.doesNotMatch(await evaluate(`saved.at(-1).html_body`),/Personal Team/);
  // Pasted HTML must retain formatting while dropping executable or remote content.
  await evaluate(`const editor=document.querySelector('#body');editor.focus();const selection=window.getSelection();selection.selectAllChildren(editor);selection.collapseToStart();const paste=new DataTransfer();paste.setData('text/html','<p><b>Pasted</b><a href="javascript:alert(1)">Unsafe</a><img src="https://tracker.invalid/pixel"><script>window.badPaste=true</script></p>');editor.dispatchEvent(new ClipboardEvent('paste',{clipboardData:paste,bubbles:true,cancelable:true}));new Promise(r=>setTimeout(r,650))`);
  assert.equal(await evaluate(`document.querySelectorAll('#body img,#body script,#body [onclick],#body a[href^="javascript:"]').length`),0);
  assert.match(await evaluate(`saved.at(-1).html_body`),/<b>Pasted<\/b>/);
  assert.equal(await evaluate(`!!window.badPaste`),false);
  // Link insertion restores the editor selection after entering the URL.
  await evaluate(`const bold=document.querySelector('#body b');const linkSelection=window.getSelection();linkSelection.selectAllChildren(bold);document.querySelector('#compose-toolbar input').dispatchEvent(new MouseEvent('mousedown',{bubbles:true}));document.querySelector('#compose-toolbar input').focus();document.querySelector('#compose-toolbar input').value='https://example.com/meeting';[...document.querySelectorAll('#compose-toolbar button')].find(b=>b.textContent==='Add link').click();new Promise(r=>setTimeout(r,650))`);
  assert.equal(await evaluate(`document.querySelector('#body a[href="https://example.com/meeting"]').textContent`),'Pasted');
  assert.match(await evaluate(`saved.at(-1).html_body`),/https:\/\/example.com\/meeting/);
  // Reload via attachment operations must preserve formatting without inserting another signature.
  await evaluate(`const files=new DataTransfer();files.items.add(new File(['attachment'],'note.txt',{type:'text/plain'}));document.querySelector('#attachment').files=files.files;document.querySelector('#attachment').dispatchEvent(new Event('change'));new Promise(r=>setTimeout(r,80))`);
  assert.equal(await evaluate(`document.querySelectorAll('#body [data-focal-signature]').length`),1);
  assert.ok(await evaluate(`document.querySelector('#body b').textContent.includes('Pasted')`));
  await evaluate(`document.querySelector('#close-compose').click();new Promise(r=>setTimeout(r,60))`);
  // The personal account's reply setting disables its signature in replies.
  await evaluate(`document.querySelector('#reply-all').click();new Promise(r=>setTimeout(r,60))`);
  assert.equal(await evaluate(`document.querySelectorAll('#body [data-focal-signature]').length`),0);
  await evaluate(`document.querySelector('#close-compose').click();new Promise(r=>setTimeout(r,60))`);

  await evaluate(`[...document.querySelectorAll('#accounts .account')].find(b=>b.title==='work@example.com').click()`);
  assert.ok(await evaluate(`[...document.querySelectorAll('#accounts .account-folder')].some(b=>b.textContent==='Empty folder')`),'Empty server folders are visible');
  await evaluate(`[...document.querySelectorAll('#accounts .account-folder')].find(b=>b.textContent==='Projects/Client A').click();new Promise(r=>setTimeout(r,70))`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='mailbox').at(-1).folder`),'account-folder:'+JSON.stringify(['work@example.com','Projects/Client A']));
  assert.equal(await evaluate(`document.querySelector('#folder-title').textContent`),'work@example.com / Projects/Client A');

  await evaluate(`mailboxSize=250;document.querySelector('#filter-all').click();new Promise(r=>setTimeout(r,70))`);
  assert.equal(await evaluate(`document.querySelectorAll('.message').length`),100,'Only one bounded page is rendered');
  await evaluate(`document.querySelector('#next-page').click();new Promise(r=>setTimeout(r,50))`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='mailbox').at(-1).offset`),100);
  assert.match(await evaluate(`document.querySelector('#count').textContent`),/101.*200 of 250/);
  await evaluate(`document.querySelector('#refresh').click();new Promise(r=>setTimeout(r,30))`);
  assert.equal(await evaluate(`!document.querySelector('#cancel').hidden`),true);
  assert.equal(await evaluate(`document.querySelector('#folders button').disabled`),false,'Folders stay usable during sync');
  await evaluate(`[...document.querySelectorAll('#folders button')].find(b=>b.textContent==='Sent').click();new Promise(r=>setTimeout(r,50))`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='mailbox').at(-1).folder`),'Sent');
  await evaluate(`resolveSync('Synced');new Promise(r=>setTimeout(r,50))`);
  await evaluate(`document.querySelectorAll('.message')[0].click();new Promise(r=>setTimeout(r,30))`);
  await evaluate(`document.querySelectorAll('.message')[1].click();new Promise(r=>setTimeout(r,30))`);
  assert.equal(await evaluate(`document.querySelectorAll('[role=tab]').length`),2,'Opening messages creates reader tabs');
  await evaluate(`document.querySelector('.tab-close').click()`);
  assert.equal(await evaluate(`document.querySelectorAll('[role=tab]').length`),1,'Tabs can be closed');
  const before=await evaluate(`document.querySelector('[aria-label="Resize account sidebar"]').getAttribute('aria-valuenow')`);
  await evaluate(`document.querySelector('[aria-label="Resize account sidebar"]').dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowRight',bubbles:true}));new Promise(r=>setTimeout(r,20))`);
  assert.equal(Number(await evaluate(`document.querySelector('[aria-label="Resize account sidebar"]').getAttribute('aria-valuenow')`)),Number(before)+16,'Pane sizes respond to keyboard controls');
  assert.equal(Number.parseInt(await evaluate(`getComputedStyle(document.querySelector('.app-shell')).getPropertyValue('--sidebar-width')`),10),Number(before)+16,'Resize styles work under the application CSP');
  await evaluate(`
    mail.html.layout='<table width="100%" style="background-color:#fff;color:#222"><tbody><tr><td><h1>Newsletter layout</h1><p>Offline formatted content</p><a href="https://example.com/newsletter">Newsletter link</a><img data-mail-image="0" alt="Remote hero"><img data-mail-image="1" alt="Inline logo"></td></tr></tbody></table><script>parent.document.body.dataset.sandboxEscape="yes"</script>';
    mail.html.images=[{index:0,alt:'Remote hero',source:'https://tracker.invalid/hero.gif',inline_data:null},{index:1,alt:'Inline logo',source:'cid:logo',inline_data:'data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7'}];
    document.querySelectorAll('.message')[0].click();new Promise(r=>setTimeout(r,80));
  `);
  assert.equal(await evaluate(`document.querySelector('.mail-frame').sandbox.value`),'allow-scripts','Email frame has no same-origin, popup, form, or navigation privileges');
  assert.equal(await evaluate(`document.body.dataset.sandboxEscape`),undefined,'Sender script cannot touch the app');
  assert.equal(await evaluate(`calls.filter(c=>c.op==='remote_images').length`),0,'Images never contact the network automatically');
  let emailFrame,emailSession;
  for(let attempt=0;attempt<60;attempt++){
    const tree=await cdp('Page.getFrameTree');
    emailFrame=tree.frameTree.childFrames?.find(f=>f.frame.url==='about:srcdoc');
    if(emailFrame)break;
    const targets=await cdp('Target.getTargets');
    const isolated=targets.targetInfos.find(t=>t.type==='iframe'&&t.url==='about:srcdoc');
    if(isolated){
      emailSession=(await cdp('Target.attachToTarget',{targetId:isolated.targetId,flatten:true})).sessionId;
      await cdp('Page.enable',{},emailSession);
      emailFrame=(await cdp('Page.getFrameTree',{},emailSession)).frameTree;break;
    }
    await new Promise(r=>setTimeout(r,50));
  }
  assert.ok(emailFrame,'Formatted email loads inside its sandbox under the real CSP');
  const world=await cdp('Page.createIsolatedWorld',{frameId:emailFrame.frame.id,worldName:'mail-tests'},emailSession);
  const inFrame=async expression=>{
    const result=await cdp('Runtime.evaluate',{expression,contextId:world.executionContextId,returnByValue:true},emailSession);
    assert.equal(result.exceptionDetails,undefined,JSON.stringify(result.exceptionDetails));return result.result.value;
  };
  assert.equal(await inFrame(`document.querySelector('h1').textContent`),'Newsletter layout');
  assert.equal(await inFrame(`getComputedStyle(document.querySelector('table')).backgroundColor`),'rgb(255, 255, 255)','Sanitized newsletter styling is displayed');
  assert.equal(await inFrame(`document.querySelector('[data-mail-image="0"]').getAttribute('src')`),null);
  assert.match(await inFrame(`document.querySelector('[data-mail-image="1"]').src`),/^data:image\/gif;base64,/,'CID images are available offline');
  assert.equal(await inFrame(`(()=>{try{parent.document.body;return 'allowed'}catch{return 'blocked'}})()`),'blocked','Frame cannot access parent DOM');
  await inFrame(`document.querySelector('a').click()`);
  await new Promise(r=>setTimeout(r,20));
  assert.equal(await evaluate(`document.querySelector('#link-preview').open`),true,'Sandbox link uses the existing destination preview');
  await evaluate(`document.querySelector('#close-link').click();document.querySelector('.load-images').click()`);
  assert.equal(await evaluate(`document.querySelector('#remote-images-confirm').open`),true);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='remote_images').length`),0);
  await evaluate(`document.querySelector('#cancel-images').click()`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='remote_images').length`),0,'Keeping images blocked does not download');
  await evaluate(`document.querySelector('.load-images').click();document.querySelector('#confirm-images').click();new Promise(r=>setTimeout(r,60))`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='remote_images').length`),1);
  assert.equal(await evaluate(`Object.hasOwn(calls.find(c=>c.op==='remote_images'),'url')`),false,'Image loader accepts message identity, never arbitrary URLs');
  assert.match(await evaluate(`document.querySelector('.mail-frame').srcdoc`),/data-mail-image="0" src="data:image\/gif/);
  await evaluate(`window.originalReaderFrame=document.querySelector('.mail-frame');statuses[0].state='syncing';statuses[0].progress={phase:'importing',completed:1234,total:2000,updated:Date.now()/1000};new Promise(r=>setTimeout(r,2200))`);
  assert.match(await evaluate(`document.querySelector('#sync-detail').textContent`),/1,234 of about 2,000/);
  assert.equal(await evaluate(`document.querySelector('.mail-frame')===originalReaderFrame`),true,'Progress polling does not reset the reader');
  await evaluate(`statuses[0].progress={phase:'checking_mail',completed:1234,total:null,updated:Date.now()/1000};new Promise(r=>setTimeout(r,2200))`);
  assert.match(await evaluate(`document.querySelector('#sync-detail').textContent`),/Checking mailbox.*1,234 messages checked/);
  await evaluate(`document.querySelector('#pause-sync').click();new Promise(r=>setTimeout(r,50))`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='pause_sync').length`),1);
  assert.equal(await evaluate(`document.querySelector('#sync-title').textContent`),'Sync paused');
  assert.equal(await evaluate(`document.querySelector('#retry-sync').textContent`),'Resume');
  await evaluate(`statuses[0].service_available=false;new Promise(r=>setTimeout(r,2200))`);
  assert.equal(await evaluate(`document.querySelector('#sync-title').textContent`),'Mail service unavailable','A stopped service is distinct from a slow provider');
  assert.equal(await evaluate(`document.querySelector('#retry-sync').hidden`),false);
  assert.equal(await evaluate(`document.querySelector('.mail-frame')===originalReaderFrame`),true);
  await evaluate(`previewMessages=null;mailboxSize=250;document.querySelector('#search').value='';document.querySelector('#search').dispatchEvent(new Event('input'));new Promise(r=>setTimeout(r,450))`);
  const readsBefore=await evaluate(`calls.filter(c=>c.op==='cached_message').length`);
  await evaluate(`document.querySelectorAll('.message-check')[0].click();document.querySelectorAll('.message-check')[1].click()`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='cached_message').length`),readsBefore,'Selecting mail does not open it');
  assert.equal(await evaluate(`document.querySelector('#selection-count').textContent`),'2 selected');
  await evaluate(`bulkPartial=true;document.querySelector('#bulk-read').click();new Promise(r=>setTimeout(r,80))`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='bulk_modify').length`),1,'Partial changes are never retried automatically');
  assert.equal(await evaluate(`calls.find(c=>c.op==='bulk_modify').targets.length`),2);
  assert.equal(await evaluate(`document.querySelector('#selection-count').textContent`),'1 selected');
  assert.match(await evaluate(`document.querySelector('#bulk-result').textContent`),/1 updated; 1 not confirmed.*Simulated failure/);
  await evaluate(`document.querySelector('#clear-selection').click();document.querySelector('#select-page').click()`);
  assert.equal(await evaluate(`document.querySelector('#selection-count').textContent`),'100 selected');
  await evaluate(`document.querySelector('#clear-selection').click();document.querySelectorAll('.message-check')[0].click();document.querySelector('#bulk-trash').click()`);
  assert.equal(await evaluate(`document.querySelector('#bulk-trash-confirm').open`),true);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='bulk_modify').length`),1,'Trash waits for confirmation');
  await evaluate(`document.querySelector('#cancel-bulk-trash').click()`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='bulk_modify').length`),1);
  await evaluate(`bulkPartial=false;document.querySelector('#bulk-trash').click();document.querySelector('#confirm-bulk-trash').click();new Promise(r=>setTimeout(r,80))`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='bulk_modify').at(-1).action`),'Trash');
  assert.equal(await evaluate(`document.querySelector('#selection-count').textContent`),'0 selected');
  await evaluate(`document.querySelector('#settings').click();new Promise(r=>setTimeout(r,30));`);
  assert.equal(await evaluate(`document.querySelector('#microsoft-sign-in').value`),'Browser','Standard browser sign-in is the default');
  await evaluate(`document.querySelector('#provider').value='Microsoft365';document.querySelector('#provider').dispatchEvent(new Event('change'));document.querySelector('#account-email').value='work@example.com';document.querySelector('#client-id').value='test-client';document.querySelector('#account-form').requestSubmit();new Promise(r=>setTimeout(r,80))`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='connect_profile').at(-1).profile.microsoft_sign_in`),'Browser');
  assert.equal(await evaluate(`document.querySelector('#account-signin-browser-area').hidden`),false);
  assert.equal(await evaluate(`document.querySelector('#account-signin-code-area').hidden`),true,'Browser authorization does not require a device code');
  assert.equal(await evaluate(`document.querySelector('#open-microsoft-browser').disabled`),false);
  await evaluate(`document.querySelector('#open-microsoft-browser').click();new Promise(r=>setTimeout(r,40))`);
  assert.match(await evaluate(`calls.filter(c=>c.op==='open_link').at(-1).url`),/^https:\/\/login\.microsoftonline\.com\/common\/oauth2\/v2\.0\/authorize/);
  await evaluate(`eventHandlers['mail-progress']({payload:'Microsoft browser response received. Verifying account authorization…'});activeAuthPrompt=null;resolveAuth('Connected work@example.com');new Promise(r=>setTimeout(r,80))`);
  assert.equal(await evaluate(`document.querySelector('#account-signin-browser-area').hidden`),true);
  await evaluate(`document.querySelector('#close-account-signin').click();document.querySelector('#microsoft-sign-in').value='DeviceCode'`);
  await evaluate(`document.querySelector('#provider').value='Microsoft365';document.querySelector('#provider').dispatchEvent(new Event('change'));document.querySelector('#account-email').value='work@example.com';document.querySelector('#client-id').value='test-client';document.querySelector('#account-form').requestSubmit();new Promise(r=>setTimeout(r,80))`);
  assert.equal(await evaluate(`authListenersReady`),true,'Sign-in subscribes before requesting a device code');
  assert.equal(await evaluate(`document.querySelector('#account-signin').open`),true,'Code is in a foreground dialog above Settings');
  assert.equal(await evaluate(`document.querySelector('#account-signin-code').value`),'TEST-CODE');
  assert.equal(await evaluate(`document.querySelector('#open-microsoft-signin').disabled`),false,'Browser action remains usable while authorization is pending');
  await evaluate(`Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async text=>{window.copiedTestCode=text;}}});document.querySelector('#copy-signin-code').click();new Promise(r=>setTimeout(r,30))`);
  assert.equal(await evaluate(`copiedTestCode`),'TEST-CODE');
  await evaluate(`document.querySelector('#open-microsoft-signin').click();document.querySelector('#status').textContent='Background status changed';new Promise(r=>setTimeout(r,40))`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='open_link').at(-1).url`),'https://microsoft.com/devicelogin');
  assert.equal(await evaluate(`document.querySelector('#account-signin-code').value`),'TEST-CODE','Status bar updates never erase the code');
  await evaluate(`eventHandlers['mail-progress']({payload:'Microsoft account authorized. Account connected.'});activeAuthPrompt=null;resolveAuth('Connected work@example.com');new Promise(r=>setTimeout(r,80))`);
  assert.equal(await evaluate(`document.querySelector('#account-signin-code-area').hidden`),true,'Successful authorization clears the code');
  assert.equal(await evaluate(`document.querySelector('#close-account-signin').hidden`),false);
  await evaluate(`document.querySelector('#close-account-signin').click();mockSignInMode='fallback';document.querySelector('#account-form').requestSubmit();new Promise(r=>setTimeout(r,2300))`);
  assert.equal(await evaluate(`document.querySelector('#account-signin-code').value`),'TEST-CODE','Polling recovers the code if its event was missed');
  await evaluate(`document.querySelector('#cancel-account-signin').click();new Promise(r=>setTimeout(r,80))`);
  assert.match(await evaluate(`document.querySelector('#account-signin-status').textContent`),/cancelled/);
  assert.equal(await evaluate(`document.querySelector('#account-signin-code').value`),'');
  await evaluate(`document.querySelector('#close-account-signin').click();mockSignInMode='error';document.querySelector('#account-form').requestSubmit();new Promise(r=>setTimeout(r,80))`);
  assert.match(await evaluate(`document.querySelector('#account-signin-status').textContent`),/could not issue a sign-in code.*public client/,'Pre-code errors remain visible above Settings');
  assert.equal(await evaluate(`document.querySelector('#account-signin-code-area').hidden`),true);
  await evaluate(`document.querySelector('#close-account-signin').click()`);
  assert.deepEqual(await evaluate(`[...document.querySelectorAll('#account-settings select')].map(s=>s.value)`),['one_year','one_year']);
  assert.equal(await evaluate(`document.querySelector('#account-settings select').options.length`),5);
  await evaluate(`const history=document.querySelector('#account-settings select');history.value='three_months';history.dispatchEvent(new Event('change'));new Promise(r=>setTimeout(r,80))`);
  assert.deepEqual(await evaluate(`calls.filter(c=>c.op==='set_sync_history').at(-1)`),{op:'set_sync_history',account:'me@example.com',history:'three_months'});
  assert.deepEqual(await evaluate(`[...document.querySelectorAll('#account-settings select')].map(s=>s.value)`),['three_months','one_year'],'History changes apply to one account only');
  await evaluate(`[...document.querySelectorAll('#account-settings button')].find(b=>b.textContent==='Load older mail').click();new Promise(r=>setTimeout(r,80))`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='set_sync_history').at(-1).history`),'six_months');
  await evaluate(`document.querySelector('#close-settings').click();document.querySelector('#older-mail').click()`);
  assert.equal(await evaluate(`document.querySelector('#preferences').open`),true,'Older mail shortcut opens account history settings');
  await evaluate(`Array.from(document.querySelectorAll('#preferences button')).filter(b=>b.textContent==='Remove').at(-1).click()`);
  assert.equal(await evaluate(`document.querySelector('#remove-account-address').textContent`),'work@example.com');
  await evaluate(`document.querySelector('#cancel-remove-account').click()`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='remove_account').length`),0);
  await evaluate(`Array.from(document.querySelectorAll('#preferences button')).filter(b=>b.textContent==='Remove').at(-1).click();document.querySelector('#confirm-remove-account').click();new Promise(r=>setTimeout(r,80))`);
  assert.equal(await evaluate(`calls.filter(c=>c.op==='remove_account').length`),1);
  assert.deepEqual(await evaluate(`accounts`),['me@example.com']);
  await evaluate(`document.querySelector('#close-settings').click();statuses[0].search_pending=123;document.querySelector('#search').value='prefix';document.querySelector('#search').dispatchEvent(new Event('input'));new Promise(r=>setTimeout(r,2300))`);
  assert.match(await evaluate(`document.querySelector('#search-status').textContent`),/123 remaining.*results may be incomplete/);
  if(process.env.FOCALMAIL_SCREENSHOT){
    await evaluate(`
      const rows=[['Maya Patel','Hiking this weekend?','Hey! Are you still up for the trail on Saturday?','10:42 AM'],['FocalDesk Team','A calmer home for your mail','A few thoughtful updates to your everyday workspace.','9:18 AM'],['GitHub','Your weekly repository digest','The latest conversations and pull requests in your projects.','8:56 AM'],['The Sunday Letter','The art of paying attention','A letter about curiosity and the things we almost miss.','Yesterday'],['Jordan Lee','Notes from our design review','Here are the notes and next steps from yesterday.','Yesterday'],['Ava Chen','Dinner on Friday?','I found a little place I think you would love.','Yesterday'],['Studio North','Your project files are ready','The final assets are ready for your review.','Tuesday'],['Alex Morgan','Re: Summer plans','A few ideas for a week away from the usual.','Tuesday']];
      previewMessages=rows.map(([sender,subject,body,date],i)=>({...mail,id:'preview-'+i,sender,subject,body,date,email:i===0?'maya@example.com':'sender'+i+'@example.com',unread:i<3,flagged:i===0,html:null}));
      previewMessages[0].body='Hey Alex,\\n\\nAre you still up for the trail on Saturday? The forecast looks beautiful, and I thought we could start early enough to catch the morning light.\\n\\nI found a route that takes us along the ridge and down to the lake. About four miles, with plenty of good places to stop.\\n\\nHow does 8:30 at the trailhead sound? I can bring coffee.\\n\\nMaya';
      document.querySelector('#filter-all').click();new Promise(r=>setTimeout(r,50));
    `);
    await evaluate(`while(document.querySelector('.tab-close'))document.querySelector('.tab-close').click();document.querySelectorAll('.message')[1].click();new Promise(r=>setTimeout(r,40))`);
    await evaluate(`document.querySelectorAll('.message')[0].click();new Promise(r=>setTimeout(r,40))`);
    const shot=await cdp('Page.captureScreenshot');await fs.writeFile(process.env.FOCALMAIL_SCREENSHOT,Buffer.from(shot.data,'base64'));
  }
});
