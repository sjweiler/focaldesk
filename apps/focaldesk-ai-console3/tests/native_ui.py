"""Exercise the real Tauri/WebKit bridge against an isolated Rust fixture backend.
No Tauri calls are mocked. The model and embedding provider are test fixtures.
"""
import base64, json, os, pathlib, shutil, signal, subprocess, tempfile, time, urllib.request, urllib.error

REPO = pathlib.Path(__file__).resolve().parents[3]
ROOT = pathlib.Path(tempfile.mkdtemp(prefix='focaldesk-console-ui-'))
PORT = 18775
children = []
session = None

def request(method, path, value=None, timeout=35):
    body = None if value is None else json.dumps(value).encode()
    req = urllib.request.Request(f'http://127.0.0.1:{PORT}{path}', data=body, method=method, headers={'Content-Type':'application/json'})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as response:
            result = json.load(response)
    except urllib.error.HTTPError as error:
        raise AssertionError(error.read().decode()) from error
    value = result.get('value')
    if isinstance(value, dict) and value.get('error'):
        raise AssertionError(value)
    return value

def wd(method, path, value=None):
    return request(method, f'/session/{session}{path}', value)

def js(script, *args):
    return wd('POST','/execute/sync',{'script':script,'args':list(args)})

def wait(predicate, label, seconds=15):
    deadline=time.monotonic()+seconds
    while time.monotonic()<deadline:
        try:
            value=predicate()
            if value:return value
        except Exception:
            pass
        time.sleep(.1)
    raise AssertionError(f'Timed out: {label}; text={js("return document.body.innerText")}')

def click_text(text, tag='button'):
    found=js(r'const e=Array.from(document.querySelectorAll(arguments[1])).find(e=>e.textContent.trim()===arguments[0]); if(e){e.click();return true;}return false;',text,tag)
    assert found, f'Missing {tag}: {text}'

def fill(selector, value):
    element=wd('POST','/element',{'using':'css selector','value':selector})
    ident=element['element-6066-11e4-a52e-4f735466cecf']
    wd('POST',f'/element/{ident}/clear',{})
    wd('POST',f'/element/{ident}/value',{'text':value})

def body_has(text):
    return text in js(r'return document.body.innerText')

def start(command, env, log):
    handle=open(ROOT/log,'wb')
    child=subprocess.Popen(command,env=env,stdout=handle,stderr=subprocess.STDOUT)
    children.append((child,handle));return child

try:
    env=os.environ.copy()
    display=subprocess.run(['systemctl','--user','show-environment'],capture_output=True,text=True,check=True).stdout
    for line in display.splitlines():
        key,_,value=line.partition('=')
        if key in {'WAYLAND_DISPLAY','DISPLAY','XDG_RUNTIME_DIR','DBUS_SESSION_BUS_ADDRESS','XDG_SESSION_TYPE'}:env[key]=value
    for kind in ['config','data','cache']:(ROOT/kind).mkdir()
    env.update({'XDG_CONFIG_HOME':str(ROOT/'config'),'XDG_DATA_HOME':str(ROOT/'data'),'XDG_CACHE_HOME':str(ROOT/'cache'),'FOCALDESK_AI_SOCKET':str(ROOT/'ai.sock'),'FOCALDESK_CONSOLE_FIXTURE_ROOT':str(ROOT),'FOCALDESK_AI_PERMISSION':'allow-session','TAURI_WEBVIEW_AUTOMATION':'true','FOCALDESK_BROWSER_SOCKET':str(ROOT/'browser.sock')})
    (ROOT/'config/focaldesk').mkdir()
    (ROOT/'config/focaldesk/ai_console.json').write_text(json.dumps({'conversations':[{'title':'Legacy fixture history','summary':'Preserved','messages':['User: legacy question','AI: legacy answer']}],'app_state':{}}))
    (ROOT/'config/focaldesk/focaldesk-server.env').write_text('KEEP_FIXTURE=true\n')
    (ROOT/'docs/nested').mkdir(parents=True)
    (ROOT/'docs/alpha.md').write_text('# Alpha fixture\nFocalDesk fixture build channel is cyan.\n')
    (ROOT/'docs/nested/beta.md').write_text('# Beta fixture\nFocalDesk fixture build channel is amber.\n')
    start([str(REPO/'target/debug/examples/console_fixture')],env,'backend.log')
    start([str(REPO/'target/debug/focaldesk-browserd')],env,'browser.log')
    wait(lambda:(ROOT/'ai.sock').exists(),'fixture socket')
    start(['/usr/bin/WebKitWebDriver',f'--port={PORT}','--host=127.0.0.1'],env,'driver.log')
    wait(lambda:request('GET','/status'),'WebKit driver')
    result=request('POST','/session',{'capabilities':{'alwaysMatch':{'webkitgtk:browserOptions':{'binary':str(REPO/'target/debug/focaldesk-ai-console'),'args':[]}}}},timeout=45)
    session=result['sessionId']
    wd('POST','/timeouts',{'script':15000,'implicit':0,'pageLoad':30000})
    wait(lambda:body_has('FocalDesk AI Console'),'console load')
    wait(lambda:body_has('AI daemon connected'),'native IPC connection')
    wait(lambda:not js(r'return document.querySelector(".new-chat").disabled'),'state ready')
    click_text('Projects')
    fill('input[aria-label="Workspace name"]','Native UI workspace')
    click_text('Create workspace')
    wait(lambda:body_has('Native UI workspace'),'workspace creation')
    click_text('Knowledge')
    fill('input[aria-label="Source path"]',str(ROOT/'docs/alpha.md'))
    click_text('Start indexing')
    wait(lambda:body_has('completed'),'document indexing job')
    assert body_has('alpha.md')
    # Change source kind using native DOM change; it drives the real React form.
    js(r'const e=document.querySelector(".knowledge-workspace select"); e.value="directory"; e.dispatchEvent(new Event("change",{bubbles:true}));')
    fill('input[aria-label="Source path"]',str(ROOT/'docs'))
    click_text('Start indexing')
    wait(lambda:body_has('beta.md'),'recursive folder indexing')
    click_text('Projects')
    js(r'const cards=Array.from(document.querySelectorAll("main .panel"));const c=cards.find(c=>c.querySelector("h3")?.textContent==="Native UI workspace");c.querySelector("input[type=checkbox]").click();')
    wait(lambda:body_has('alpha.md'),'indexed source selection')
    js(r'const c=Array.from(document.querySelectorAll("main .panel")).find(c=>c.querySelector("h3")?.textContent==="Native UI workspace");c.querySelector(".source-selector input").click();')
    js(r'const c=Array.from(document.querySelectorAll("main .panel")).find(c=>c.querySelector("h3")?.textContent==="Native UI workspace");c.querySelector(".service-toolbar button").click();')
    fill('textarea[aria-label="Message"]','hello fixture')
    js(r'document.querySelector("button[aria-label=\"Send message\"]").click();')
    wait(lambda:body_has('Project sources: 1'),'project-scoped request')
    js(r'document.querySelector("button[aria-label=\"Rename conversation\"]").click();')
    fill('[role="dialog"] input','Renamed native conversation')
    click_text('Save')
    wait(lambda:body_has('Renamed native conversation'),'conversation rename')
    fill('textarea[aria-label="Message"]','long fixture')
    js(r'document.querySelector("button[aria-label=\"Send message\"]").click();')
    wait(lambda:body_has('Fixture streaming line'),'partial streamed output')
    js(r'document.querySelector("button[aria-label=\"Stop generating\"]").click();')
    wait(lambda:body_has('Generation stopped or interrupted.'),'generation stopped')
    click_text('History')
    click_text('Import old console history')
    wait(lambda:body_has('Legacy fixture history'),'legacy history migration')
    # Management pages read real fixture state through the Tauri command adapter.
    for tab in ['Agents','Models','Integrations','Controls']:
        click_text(tab)
        time.sleep(.3)
        assert not body_has('Unknown service section')
    for panel in ['Workflows','Mission Control','Packages','Scenarios','Context','Events','Attention','Capabilities','Connectors']:
        js(r'const nav=document.querySelector("nav[aria-label=\"Control panels\"]");const e=Array.from(nav.querySelectorAll("button")).find(b=>b.textContent===arguments[0]);e.click();',panel)
        time.sleep(.2)
        assert body_has('FocalDesk AI Console'), f'{panel} render failed'
        assert not js(r'return Array.from(document.querySelectorAll("main [role=alert]")).some(e=>e.textContent.trim())'), f'{panel} service read failed: '+js('return document.body.innerText')
    click_text('Settings', 'button') if js(r'return Array.from(document.querySelectorAll("button")).some(b=>b.textContent.trim()==="Settings")') else js(r'document.querySelector("button[aria-label=Settings]").click();')
    wait(lambda:body_has('Provider configuration'),'provider settings page')
    fill('input[placeholder="ollama"]','ollama')
    click_text('Save configuration')
    wait(lambda:body_has('Provider settings saved.'),'provider config saved')
    assert 'KEEP_FIXTURE=true' in (ROOT/'config/focaldesk/focaldesk-server.env').read_text()
    assert 'FOCALDESK_AI_PROVIDER="ollama"' in (ROOT/'config/focaldesk/focaldesk-server.env').read_text()
    click_text('Browser')
    wait(lambda:body_has('Browser service ready'),'browser IPC connection')
    fill('input[aria-label="Website URL"]','https://example.com/?fixture=one')
    click_text('Go')
    wait(lambda:body_has('Example Domain'),'Chromium rendered page',40)
    wait(lambda:not js(r'return document.querySelector(".browser-address button[type=submit]")?.disabled ?? document.querySelector(".browser-address button:last-child").disabled'),'browser idle')
    fill('input[aria-label="Website URL"]','https://example.com/?fixture=two')
    click_text('Go')
    wait(lambda:js(r'return Array.from(document.querySelectorAll(".browser-workspace p")).some(e=>e.textContent==="https://example.com/?fixture=two")'),'navigate',40)
    js(r'document.querySelector("button[title=Back]").click();')
    wait(lambda:js(r'return document.querySelector("input[aria-label=\"Website URL\"]").value.endsWith("fixture=one")'),'back',40)
    wait(lambda:not body_has('Browser operation in progress'),'back completed',40)
    js(r'document.querySelector("button[title=Forward]").click();')
    wait(lambda:js(r'return document.querySelector("input[aria-label=\"Website URL\"]").value.endsWith("fixture=two")'),'forward',40)
    wait(lambda:not body_has('Browser operation in progress'),'forward completed',40)
    js(r'document.querySelector("button[title=Reload]").click();')
    wait(lambda:not body_has('Browser operation in progress'),'reload',40)
    wait(lambda:not body_has('Browser operation in progress'),'reload completed',40)
    click_text('Screenshot')
    wait(lambda:js(r'return !!document.querySelector(".browser-screenshot")?.src.startsWith("data:image/png")'),'Chromium screenshot',40)
    wait(lambda:not body_has('Browser operation in progress'),'screenshot completed',40)
    click_text('Close page')
    click_text('Chat')
    time.sleep(.8)
    state_path=ROOT/'data/focaldesk/ai-console/state.json'
    wait(state_path.exists,'disk-backed history')
    state=json.loads(state_path.read_text())
    assert any(c['title']=='Renamed native conversation' for c in state['chats'])
    assert next(p for p in state['projects'] if p['name']=='Native UI workspace')['sources']
    screenshot=wd('GET','/screenshot')
    (ROOT/'console.png').write_bytes(base64.b64decode(screenshot))
    print(json.dumps({'result':'passed','checks':['native IPC','workspace CRUD','document indexing','recursive folder indexing','project RAG scope','streamed deltas','Stop','conversation rename','legacy history migration','control panels','provider config preservation','disk-backed history','Chromium navigation and screenshot'],'screenshot':str(ROOT/'console.png'),'fixture_root':str(ROOT)}),flush=True)
finally:
    if session:
        try:wd('DELETE','')
        except Exception:pass
    for child,handle in reversed(children):
        if child.args[0].endswith("focaldesk-browserd"):child.send_signal(signal.SIGINT)
        else:child.terminate()
        try:child.wait(timeout=5)
        except subprocess.TimeoutExpired:child.kill();child.wait()
        handle.close()
    # Keep fixture logs and screenshot for review; no user data is present.
