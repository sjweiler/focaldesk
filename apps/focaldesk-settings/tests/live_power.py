"""Read-only production IPC check using installed Settings and the running power daemon.

Opens Power, reads GetSnapshot and saves a window screenshot. Never presses Apply
or invokes suspend/hibernate/reboot/power-off/profile/volume/display operations.
Private XDG config/data/cache/state keep the test's UI preferences isolated.
"""
import base64
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.request

ROOT = Path(tempfile.mkdtemp(prefix='focaldesk-live-power-'))
PORT = 18779
session = None
driver = None


def request(method, path, value=None):
    data = None if value is None else json.dumps(value).encode()
    req = urllib.request.Request(f'http://127.0.0.1:{PORT}{path}', data=data, method=method, headers={'Content-Type':'application/json'})
    with urllib.request.urlopen(req, timeout=30) as response:
        value = json.load(response)['value']
    if isinstance(value, dict) and value.get('error'):
        raise AssertionError(value)
    return value


def wd(method, path, value=None):
    return request(method, f'/session/{session}{path}', value)


def js(script):
    return wd('POST','/execute/sync',{'script':script,'args':[]})


try:
    env = os.environ.copy()
    result = subprocess.run(['systemctl','--user','show-environment'], capture_output=True, text=True, check=True)
    for line in result.stdout.splitlines():
        key, _, value = line.partition('=')
        if key in {'WAYLAND_DISPLAY','DISPLAY','XDG_RUNTIME_DIR','DBUS_SESSION_BUS_ADDRESS','XDG_SESSION_TYPE'}:
            env[key] = value
    for kind in ['config','data','cache','state']:
        (ROOT/kind).mkdir()
        env[f'XDG_{kind.upper()}_HOME'] = str(ROOT/kind)
    env['TAURI_WEBVIEW_AUTOMATION'] = 'true'
    env.pop('FOCALDESK_POWER_SOCKET_PATH',None)
    log = open(ROOT/'webdriver.log','wb')
    driver = subprocess.Popen(['WebKitWebDriver',f'--port={PORT}','--host=127.0.0.1'], env=env,stdout=log,stderr=subprocess.STDOUT)
    for _ in range(100):
        try:
            if request('GET','/status'): break
        except OSError: time.sleep(.1)
    session = request('POST','/session',{'capabilities':{'alwaysMatch':{'webkitgtk:browserOptions':{'binary':'/usr/local/bin/focaldesk-settings-next','args':['--panel','power']}}}})['sessionId']
    wd('POST','/window/rect',{'width':1120,'height':900})
    deadline = time.monotonic()+30
    while time.monotonic()<deadline:
        if 'Active performance profile' in js('return document.body.innerText'): break
        time.sleep(.2)
    content = js('return document.body.innerText')
    assert 'Active performance profile' in content, content
    assert 'Connection reset by peer' not in content, content
    report = wd('POST','/execute/async',{'script':"const done=arguments[arguments.length-1];window.__TAURI_INTERNALS__.invoke('get_power_status').then(value=>done({ok:true,snapshot:value}),error=>done({ok:false,error:String(error)}));",'args':[]})
    assert report['ok'], report
    (ROOT/'status.json').write_text(json.dumps(report,indent=2))
    (ROOT/'power-live.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    print('PASS: installed Settings reads live power status through production IPC; no power actions invoked')
    print(json.dumps({'battery_count':len(report['snapshot']['batteries']),'performance_profile':report['snapshot']['performance_profile']}))
finally:
    if session:
        try: request('DELETE',f'/session/{session}')
        except Exception: pass
    if driver:
        driver.terminate()
        driver.wait(timeout=10)
    print(f'Artifacts: {ROOT}')
