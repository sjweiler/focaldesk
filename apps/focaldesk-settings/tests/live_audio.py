"""Read-only native Sound validation in the user's existing desktop session.

Only opens the Sound page and sends Tab navigation. Never changes a device,
volume, mute state, display mode, or starts a microphone level test.
"""
import base64
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.request

REPO = Path(__file__).resolve().parents[3]
ROOT = Path(tempfile.mkdtemp(prefix='focaldesk-live-sound-'))
PORT = 18777
session = None
driver = None


def request(method, path, value=None):
    data = None if value is None else json.dumps(value).encode()
    req = urllib.request.Request(f'http://127.0.0.1:{PORT}{path}', data=data, method=method, headers={'Content-Type':'application/json'})
    with urllib.request.urlopen(req, timeout=40) as response: result = json.load(response)
    value = result.get('value')
    if isinstance(value, dict) and value.get('error'): raise AssertionError(value)
    return value


def wd(method, path, value=None): return request(method, f'/session/{session}{path}', value)
def js(source): return wd('POST','/execute/sync',{'script':source,'args':[]})


try:
    env = os.environ.copy()
    result = subprocess.run(['systemctl','--user','show-environment'], capture_output=True, text=True, check=True)
    for line in result.stdout.splitlines():
        key, _, value = line.partition('=')
        if key in {'WAYLAND_DISPLAY','DISPLAY','XDG_RUNTIME_DIR','DBUS_SESSION_BUS_ADDRESS','XDG_SESSION_TYPE'}: env[key] = value
    for kind in ['config','data','cache']:
        (ROOT/kind).mkdir()
        env[f'XDG_{kind.upper()}_HOME'] = str(ROOT/kind)
    env['TAURI_WEBVIEW_AUTOMATION'] = 'true'
    log = open(ROOT/'webdriver.log','wb')
    driver = subprocess.Popen(['WebKitWebDriver',f'--port={PORT}','--host=127.0.0.1'],env=env,stdout=log,stderr=subprocess.STDOUT)
    for _ in range(100):
        try:
            if request('GET','/status'): break
        except OSError: time.sleep(.1)
    session = request('POST','/session',{'capabilities':{'alwaysMatch':{'webkitgtk:browserOptions':{'binary':str(REPO/'target/debug/focaldesk-settings-next'),'args':[]}}}})['sessionId']
    wd('POST','/window/rect',{'width':1120,'height':900})
    deadline = time.monotonic()+20
    while time.monotonic()<deadline:
        if js("const e=Array.from(document.querySelectorAll('nav button')).find(e=>e.textContent.trim()==='♫Sound');if(e){e.click();return true;}return false"): break
        time.sleep(.1)
    while time.monotonic()<deadline:
        if js("return !!document.getElementById('sound-output')"): break
        time.sleep(.1)
    assert js("return !!document.getElementById('sound-output')"), js('return document.body.innerText')
    assert js("return document.getElementById('microphone-level').value") == 0
    assert 'Not listening' in js('return document.body.innerText')
    report = js("return {device_pixel_ratio:window.devicePixelRatio,output_choices:document.querySelectorAll('#sound-output option:not([disabled])').length,input_choices:document.querySelectorAll('#sound-input option:not([disabled])').length,server:document.querySelector('.audio-footer').innerText,microphone_test_active:!!document.querySelector('.microphone-test.recording')}")
    (ROOT/'sound-live.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    expected = set()
    if report['output_choices']: expected.update({'sound-output','sound-output-volume','sound-output-mute'})
    if report['input_choices']: expected.update({'sound-input','sound-input-volume','sound-input-mute'})
    focused = []
    for _ in range(24):
        wd('POST','/actions',{'actions':[{'type':'key','id':'keyboard','actions':[{'type':'keyDown','value':'\ue004'},{'type':'keyUp','value':'\ue004'}]}]})
        active = js('return {tag:document.activeElement.tagName,id:document.activeElement.id,outline:getComputedStyle(document.activeElement).outlineStyle}')
        focused.append(active)
        expected.discard(active['id'])
        if not expected: break
    assert not expected, f'Audio controls missing from Tab navigation: {expected}'
    report['keyboard_focus'] = focused
    assert report['microphone_test_active'] is False
    assert 'Not listening' in js('return document.body.innerText')
    (ROOT/'sound-keyboard.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    (ROOT/'report.json').write_text(json.dumps(report,indent=2))
    print('PASS: live audio inventory, native WebKit rendering, keyboard focus, and fractional-scale observation; no audio/display changes or microphone capture')
    print(json.dumps(report))
finally:
    if session:
        try: request('DELETE',f'/session/{session}')
        except Exception: pass
    if driver:
        driver.terminate(); driver.wait(timeout=10)
    print(f'Artifacts: {ROOT}')
