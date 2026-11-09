const {test} = require('node:test');
const assert = require('node:assert/strict');
const {spawn} = require('node:child_process');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const {createServer} = require('node:http');

test('canvas editing, draft persistence, reopen, and native-only publishing', async t => {
  const profile=await fs.mkdtemp(path.join(os.tmpdir(),'automation-studio-web-'));
  const chrome=spawn(process.env.CHROME_BIN||'google-chrome',['--headless','--no-sandbox','--disable-gpu','--window-size=1480,940','--no-first-run','--no-default-browser-check','--remote-debugging-port=0',`--user-data-dir=${profile}`,'about:blank'],{stdio:'ignore'});
  let socket;
  t.after(async()=>{if(socket && process.env.STUDIO_SCREENSHOT){try {const shot=await cdp('Page.captureScreenshot',{format:'png'});await fs.writeFile(process.env.STUDIO_SCREENSHOT,Buffer.from(shot.data,'base64'));}catch{}}socket?.close();chrome.kill();await new Promise(r=>chrome.exitCode!==null?r():chrome.once('exit',r));await fs.rm(profile,{recursive:true,force:true,maxRetries:5});});
  let port;
  for(let i=0;i<100;i++){try{port=(await fs.readFile(path.join(profile,'DevToolsActivePort'),'utf8')).split('\n')[0];break;}catch{await new Promise(r=>setTimeout(r,100));}}
  assert.ok(port,'Chrome debugger started');
  const tabs=await (await fetch(`http://127.0.0.1:${port}/json`)).json();
  socket=new WebSocket(tabs[0].webSocketDebuggerUrl);
  await new Promise((resolve,reject)=>{socket.onopen=resolve;socket.onerror=reject;});
  let next=0;const pending=new Map();
  socket.onmessage=e=>{const m=JSON.parse(e.data);if(m.id){const p=pending.get(m.id);pending.delete(m.id);m.error?p.reject(m.error):p.resolve(m.result);}};
  const cdp=(method,params={})=>new Promise((resolve,reject)=>{const id=++next;pending.set(id,{resolve,reject});socket.send(JSON.stringify({id,method,params}));});
  const evaluate=async expression=>{const r=await cdp('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});assert.equal(r.exceptionDetails,undefined,JSON.stringify(r.exceptionDetails));return r.result.value;};
  const wait=async expression=>{for(let i=0;i<100;i++){if(await evaluate(expression))return;await new Promise(r=>setTimeout(r,50));}assert.fail(`Timed out: ${expression}; status=${await evaluate("document.querySelector('.status-bar')?.textContent")}`);};
  const dist=path.resolve(__dirname,'../dist');
  const server=createServer(async(req,res)=>{try{const pathname=new URL(req.url,'http://localhost').pathname;const file=path.resolve(dist,'.'+(pathname==='/'?'/index.html':pathname));if(!file.startsWith(dist+path.sep)){res.writeHead(403);res.end();return;}res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(await fs.readFile(file));}catch{res.writeHead(404);res.end();}});
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  t.after(()=>new Promise(resolve=>{server.closeAllConnections();server.close(resolve);}));
  await cdp('Page.enable');
  await cdp('Page.navigate',{url:`http://127.0.0.1:${server.address().port}/`});
  await wait(`document.querySelectorAll('.flow-block').length===5`);
  assert.equal(await evaluate(`document.querySelectorAll('.svelte-flow__edge').length`),4);
  await evaluate(`document.querySelector('.form-fields input').value='Studio browser test';document.querySelector('.form-fields input').dispatchEvent(new Event('input',{bubbles:true}));`);
  await wait(`document.querySelector('.draft-state').textContent.includes('Unsaved')`);
  await evaluate(`Array.from(document.querySelectorAll('button')).find(b=>b.textContent==='Save draft').click()`);
  await wait(`document.querySelector('.status-bar').textContent.includes('Draft saved')`);
  const stored=await evaluate(`Object.values(JSON.parse(localStorage.getItem('focaldesk-studio-drafts')))[0]`);
  assert.equal(stored.nodes.find(n=>n.id==='notify').data.title,'Studio browser test');
  assert.equal(stored.edges.length,4);
  await evaluate(`Array.from(document.querySelectorAll('.palette-item')).find(b=>b.textContent.includes('Wi-Fi')).click()`);
  await wait(`document.querySelectorAll('.flow-block').length===6`);
  await evaluate(`document.querySelector('.delete-button').click()`);
  await wait(`document.querySelectorAll('.flow-block').length===5`);
  await evaluate(`document.querySelector('button[title="Saved workflows"]').click()`);
  await wait(`!!document.querySelector('.draft-row')`);
  await evaluate(`document.querySelector('.draft-row').click()`);
  await wait(`document.querySelector('.status-bar').textContent.includes('Draft opened')`);
  await evaluate(`document.querySelector('.primary').click()`);
  await wait(`document.querySelector('.status-bar').textContent.includes('native FocalDesk app')`);
  assert.equal(await evaluate(`document.querySelectorAll('.flow-block').length`),5);
  // Check real node dragging updates persisted coordinates.
  const box=await evaluate(`(()=>{const r=document.querySelector('.flow-block').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})()`);
  await cdp('Input.dispatchMouseEvent',{type:'mouseMoved',x:box.x,y:box.y});
  await cdp('Input.dispatchMouseEvent',{type:'mousePressed',x:box.x,y:box.y,button:'left',clickCount:1});
  for(let i=1;i<=5;i++)await cdp('Input.dispatchMouseEvent',{type:'mouseMoved',x:box.x+10*i,y:box.y+5*i,button:'left',buttons:1});
  await cdp('Input.dispatchMouseEvent',{type:'mouseReleased',x:box.x+50,y:box.y+25,button:'left',clickCount:1});
  // Svelte Flow suppresses the click immediately following a drag.
  await new Promise(r=>setTimeout(r,100));
  await evaluate(`Array.from(document.querySelectorAll('button')).find(b=>b.textContent==='Save draft').click()`);
  await wait(`document.querySelector('.status-bar').textContent.includes('Draft saved')`);
  const moved=await evaluate(`Object.values(JSON.parse(localStorage.getItem('focaldesk-studio-drafts')))[0].nodes[0].position`);
  assert.notDeepEqual(moved,stored.nodes[0].position);
  await evaluate(`document.querySelector('.svelte-flow__node[data-id="notify"]').dispatchEvent(new MouseEvent('click',{bubbles:true}))`);
  if(process.env.STUDIO_SCREENSHOT){const shot=await cdp('Page.captureScreenshot',{format:'png'});await fs.writeFile(process.env.STUDIO_SCREENSHOT,Buffer.from(shot.data,'base64'));}
});
