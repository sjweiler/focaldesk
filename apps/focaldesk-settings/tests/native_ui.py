"""Real Tauri/WebKit integration against an isolated compositor IPC fixture.

Build frontend and focaldesk-settings-next first. Requires WebKitWebDriver and
an available Wayland/X11 session. Artifacts stay in the printed /tmp directory.
"""
import base64
import copy
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import tomllib
import urllib.request
from audio_fixture import install as install_audio_fixture
from connectivity_fixture import install as install_connectivity_fixture
from calendar_fixture import CalendarFixture
from printer_fixture import install as install_printer_fixture
from applications_fixture import install as install_applications_fixture, EDITOR, OTHER
from icc_fixture import write_srgb

REPO = Path(__file__).resolve().parents[3]
ROOT = Path(tempfile.mkdtemp(prefix="focaldesk-settings-ui-"))
PORT = 18776
children = []
session = None
requests = []
reject = False
logout_reject = False
deny_displays = os.environ.get("FOCALDESK_SETTINGS_TEST_SCOPE") == "connection"


def request(method, path, value=None):
    body = None if value is None else json.dumps(value).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:{PORT}{path}", data=body, method=method,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=45) as response:
        result = json.load(response)
    value = result.get("value")
    if isinstance(value, dict) and value.get("error"):
        raise AssertionError(value)
    return value


def wd(method, path, value=None):
    return request(method, f"/session/{session}{path}", value)


def js(script):
    return wd("POST", "/execute/sync", {"script": script, "args": []})


def invoke(command, args, ok=True):
    result=wd('POST','/execute/async',{'script':"const done=arguments[arguments.length-1];window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(value=>done({ok:true,value}),error=>done({ok:false,message:String(error)}));",'args':[command,args]})
    assert result['ok'] is ok, result
    return result['value'] if ok else result['message']


def wait(predicate, label):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.1)
    raise AssertionError(f"Timed out: {label}")


def text():
    return js("return document.body.innerText")


def button(label):
    assert js("const e=Array.from(document.querySelectorAll('button')).find(e=>e.textContent.trim()==="
              + json.dumps(label) + ");if(!e||e.disabled)return false;e.click();return true;"), label


def aria_button(label):
    selector = json.dumps(f'button[aria-label="{label}"]')
    wait(lambda: js(f'const e=document.querySelector({selector});return !!e&&!e.disabled'), label+' ready')
    js(f'document.querySelector({selector}).click()')


display_state = {"hardware_modes": True, "preview": None, "outputs": [{
    "config": {"connector":"DP-1", "enabled":True, "x":0, "y":0, "width":1920, "height":1080,
               "refresh_mhz":60000, "scale":1, "transform":"Normal", "role":"Normal", "primary":True,
               "color_profile":"auto", "icc_profile_path":None, "hdr_requested":False, "hdr_enabled":False},
    "make":"FocalDesk", "model":"Studio Display", "hdr_supported":True, "hdr_active":False,
    "wide_gamut_active":False, "icc_lut_fallback_active":False,
    "modes":[{"width":1920,"height":1080,"refresh_mhz":60000},
             {"width":1920,"height":1080,"refresh_mhz":120000},
             {"width":2560,"height":1440,"refresh_mhz":60000}],
}]}
display_previous = None
display_deadline = 0
display_patterns = {}
trial_sequence = 0
theme_state={'preview_active':False,'applied_revision':0,'owner':None,'deadline':0}
theme_reject=False


def theme_response(payload):
    if theme_state['preview_active'] and time.monotonic()>=theme_state['deadline']:
        theme_state.update(preview_active=False,owner=None)
    if payload['type']=='ThemeEditor':
        if theme_reject: return {'status':'Error','message':'Fixture rejected theme change'}
        command=payload['command'];kind=command['command']
        if kind=='preview_owned':
            if theme_state['preview_active'] and theme_state['owner']!=command['token']: return {'status':'Error','message':'Another theme editor has an active preview'}
            theme_state.update(preview_active=True,owner=command['token'],deadline=time.monotonic()+30)
        elif kind=='revert_owned':
            if theme_state['preview_active'] and theme_state['owner']!=command['token']: return {'status':'Error','message':'Preview belongs to another editor'}
            theme_state.update(preview_active=False,owner=None)
        elif kind=='apply_owned':
            if command['expected_revision']!=theme_state['applied_revision']: return {'status':'Error','message':'Session theme changed in another editor'}
            theme_state.update(preview_active=False,owner=None,applied_revision=theme_state['applied_revision']+1)
    return {'status':'ThemeEditorStatus','protocol_version':1,'preview_active':theme_state['preview_active'],'applied_revision':theme_state['applied_revision'],'gradient_rendering':True,'semantic_rendering':True,'wallpaper_processing':True,'layout_metrics':True,'typography_metrics':True,'contrast_issue_count':0}

input_state = {
    "pointer_speed": 1.0, "natural_scroll": False,
    "workspace_swipe_fingers": 3, "workspace_swipe_distance": 120.0,
    "devices": {}, "keyboard_layout": "us", "keyboard_variant": "",
    "keyboard_model": "", "keyboard_options": "",
    "keyboard_repeat_delay_ms": 200, "keyboard_repeat_rate": 25,
    "keybindings": {}, "accessibility": {"sticky_keys": False, "slow_keys": False,
        "slow_keys_delay_ms": 500, "bounce_keys": False, "bounce_keys_delay_ms": 300,
        "sticky_keys_shortcut": True, "visual_feedback": True},
}


def input_response(payload):
    global input_state
    if payload['type'] == 'SetInputConfiguration':
        if payload['expected'] != input_state:
            return {'status': 'Error', 'message': 'Input preferences changed in another client.'}
        input_state = copy.deepcopy(payload['input'])
        document = json.loads(settings.read_text())
        document['input'] = input_state
        settings.write_text(json.dumps(document))
    return {'status': 'InputConfiguration', 'configuration': {
        'input': input_state, 'saved_input': input_state, 'devices': [],
        'hardware_devices': False,
        'layouts': [{'id': 'us', 'label': 'English (US)'}, {'id': 'de', 'label': 'German'}],
        'shortcuts': [{'action': 'launch_terminal', 'label': 'Launch terminal', 'default': 'Super+Return'}],
    }}


def display_response(payload):
    global display_previous, display_deadline, trial_sequence
    kind = payload["type"]
    if display_state["preview"] and time.monotonic() >= display_deadline:
        display_state["outputs"] = display_previous
        display_state["preview"] = None
        display_patterns.clear()
    if kind=='SetDisplayTrialCalibrationPattern':
        if not display_state['preview'] or payload['preview_id']!=display_state['preview']['id']:
            return {'status':'Error','message':'This display trial is no longer current.'}
        target=next((output for output in display_state['outputs'] if output['config']['connector']==payload['connector']),None)
        if not target or not target['hdr_active']:
            return {'status':'Error','message':'HDR is not active on this connector.'}
        display_patterns[payload['connector']]=payload['pattern']
        return {'status':'Ok'}
    if kind == "PreviewDisplays":
        assert display_state["preview"] is None
        display_previous = copy.deepcopy(display_state["outputs"])
        for output, config in zip(display_state["outputs"], payload["outputs"]):
            output["config"] = config
            output["hdr_active"] = config["hdr_requested"]
        trial_sequence += 1
        display_deadline = time.monotonic() + 20
        display_state["preview"] = {"id":trial_sequence,"phase":"confirming","remaining_ms":20000}
    elif kind == "RevertDisplays":
        assert payload["preview_id"] == display_state["preview"]["id"]
        display_state["outputs"] = display_previous
        display_state["preview"] = None
        display_patterns.clear()
    elif kind == "ConfirmDisplays":
        assert payload["preview_id"] == display_state["preview"]["id"]
        document = json.loads(settings.read_text())
        document.setdefault("displays", {})["outputs"] = [output["config"] for output in display_state["outputs"]]
        settings.write_text(json.dumps(document))
        display_state["preview"] = None
        display_patterns.clear()
    if display_state["preview"]:
        display_state["preview"]["remaining_ms"] = max(0, int((display_deadline - time.monotonic()) * 1000))
    return {"status":"DisplayConfiguration", "configuration":display_state}


def fixture():
    while True:
        connection, _ = server.accept()
        with connection:
            if deny_displays:
                continue
            chunks = []
            while block := connection.recv(65536):
                chunks.append(block)
            payload = json.loads(b"".join(chunks))["payload"]
            requests.append(payload)
            if payload['type'] == 'ExecuteDesktopAction':
                assert payload['action'] == {'action':'logout'}
                response = {'status':'Error','message':'Fixture logout rejected'} if logout_reject else {'status':'Ok'}
            elif payload['type'] in {'GetThemeEditorStatus','ThemeEditor'}:
                response=theme_response(payload)
            elif payload['type'] in {'GetInputConfiguration', 'SetInputConfiguration'}:
                response = input_response(payload)
            elif payload["type"] != "SetConfig":
                response = display_response(payload)
            elif reject:
                response = {"status": "Error", "message": "Fixture rejected appearance"}
            else:
                document = json.loads(settings.read_text())
                document["desktop_config"] = payload["config"]
                settings.write_text(json.dumps(document))
                response = {"status": "Ok"}
            connection.sendall(json.dumps({"protocol_version": 1, "payload": response}).encode())


operation_requests = []
operation_reject = False
power_close = os.environ.get("FOCALDESK_SETTINGS_TEST_SCOPE") == "power"
def operations_fixture(path, kind):
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(path)); listener.listen()
    def serve():
        while True:
            connection, _ = listener.accept()
            with connection:
                if kind == 'power' and power_close:
                    continue
                raw = b''
                while chunk := connection.recv(65536): raw += chunk
                payload = json.loads(raw)['payload']
                operation_requests.append((kind, payload))
                if operation_reject:
                    response = {'status':'Error','message':'Fixture authorization rejected'}
                elif payload['type'] == 'GetSnapshot':
                    response = {'status':'PowerSnapshot','snapshot':{'batteries':[{'name':'Fixture battery','percentage':73,'state':'discharging'}],'line_power_online':False,'performance_profile':'balanced','captured_at_unix_ms':0}}
                else: response = {'status':'Ok'}
                connection.sendall(json.dumps({'protocol_version':1,'payload':response}).encode())
    threading.Thread(target=serve, daemon=True).start()
    return listener


try:
    env = os.environ.copy()
    audio_root = ROOT / "audio-fixture"
    audio_bin = install_audio_fixture(audio_root)
    env["PATH"] = str(audio_bin) + os.pathsep + env["PATH"]
    env["FOCALDESK_AUDIO_FIXTURE"] = str(audio_root)
    connectivity_root = ROOT / 'connectivity-fixture'
    connectivity_bin = install_connectivity_fixture(connectivity_root)
    env['PATH'] = str(connectivity_bin) + os.pathsep + env['PATH']
    env['FOCALDESK_CONNECTIVITY_FIXTURE'] = str(connectivity_root)
    calendar_fixture=CalendarFixture(ROOT/'calendar-fixture')
    env['PATH']=str(calendar_fixture.binary)+os.pathsep+env['PATH']
    env['FOCALD_CONNECTORS_SOCKET']=str(calendar_fixture.path)
    env['FOCALDESK_CALENDAR_FIXTURE']=str(calendar_fixture.root)
    printer_root=ROOT/'printer-fixture'
    printer_bin=install_printer_fixture(printer_root)
    env['PATH']=str(printer_bin)+os.pathsep+env['PATH']
    env['FOCALDESK_PRINTER_FIXTURE']=str(printer_root)
    display = ""
    if not (env.get("WAYLAND_DISPLAY") or env.get("DISPLAY")):
        display = subprocess.run(["systemctl", "--user", "show-environment"], capture_output=True, text=True, check=False).stdout
    for line in display.splitlines():
        key, _, value = line.partition("=")
        if key in {"WAYLAND_DISPLAY", "DISPLAY", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "XDG_SESSION_TYPE"}:
            env[key] = value
    for kind in ["config", "data", "cache", "state"]:
        (ROOT / kind).mkdir()
    (ROOT / "config/focaldesk").mkdir()
    settings = ROOT / "config/focaldesk/settings.json"
    settings.write_text(json.dumps({"desktop_config": {
        "appearance": {"theme": "Default", "font_scale": 1, "glow_strength": 0.75, "output_focus_glow": True},
        "dock": {"visibility": "autohide"},
    }, "fixture_unrelated": "preserved"}))
    original = settings.read_text()
    env.update({"XDG_CONFIG_HOME": str(ROOT / "config"), "XDG_DATA_HOME": str(ROOT / "data"),
                "FOCALDESK_SETTINGS_SOCKET_PATH": str(ROOT / 'unused-settings.sock'),
                "FOCALDESK_POWER_SOCKET_PATH": str(ROOT / 'power.sock'),
                "FOCALDESK_NOTIFICATIONS_SOCKET_PATH": str(ROOT / 'notifications.sock'),
                "XDG_STATE_HOME": str(ROOT / "state"),
                "XDG_CACHE_HOME": str(ROOT / "cache"), "TAURI_WEBVIEW_AUTOMATION": "true",
                "FOCALDESK_DESKTOP_SOCKET_PATH": str(ROOT / "desktop.sock")})
    power_server = operations_fixture(ROOT / "power.sock", "power")
    notification_server = operations_fixture(ROOT / "notifications.sock", "notification")
    application_canary = install_applications_fixture(ROOT, env)
    debug_log = ROOT/'fixture-focaldesk.log'
    debug_log.write_text('fixture-private-log-contents-must-not-enter-webview\n')
    env['FOCALDESK_LOG_FILE'] = str(debug_log)
    server = socket.socket(socket.AF_UNIX)
    server.bind(str(ROOT / "desktop.sock"))
    server.listen()
    threading.Thread(target=fixture, daemon=True).start()
    log = open(ROOT / "webdriver.log", "wb")
    driver = subprocess.Popen(["WebKitWebDriver", f"--port={PORT}", "--host=127.0.0.1"], env=env, stdout=log, stderr=subprocess.STDOUT)
    children.append(driver)
    for _ in range(100):
        try:
            if request("GET", "/status"):
                break
        except OSError:
            time.sleep(0.1)
    result = request("POST", "/session", {"capabilities": {"alwaysMatch": {
        "webkitgtk:browserOptions": {"binary": os.environ.get("FOCALDESK_SETTINGS_TEST_BINARY", str(REPO / "target/debug/focaldesk-settings-next")), "args": []},
    }}})
    session = result["sessionId"]
    wd("POST", "/window/rect", {"width": 1120, "height": 900})
    wait(lambda: "Choose your atmosphere" in text(), "theme catalog")
    if os.environ.get('FOCALDESK_SETTINGS_TEST_SCOPE') == 'power':
        js("document.querySelector('nav button[aria-label=Power]').click()")
        wait(lambda:'Update and restart the FocalDesk power service' in text(),'power connection explanation')
        assert js("return Array.from(document.querySelectorAll('button')).filter(button=>['Suspend…','Hibernate…','Restart…','Shut down…'].includes(button.textContent.trim())).every(button=>button.disabled)")
        power_close = False
        button('Refresh status')
        wait(lambda:'Fixture battery' in text(),'power reconnect snapshot')
        assert js("return Array.from(document.querySelectorAll('button')).filter(button=>['Suspend…','Hibernate…','Restart…','Shut down…'].includes(button.textContent.trim())).every(button=>!button.disabled)")
        power_close = True
        button('Refresh status')
        wait(lambda:'Update and restart the FocalDesk power service' in text(),'later power disconnect')
        assert 'Fixture battery' not in text()
        power_close = False
        button('Refresh status')
        wait(lambda:'Fixture battery' in text(),'second power reconnect')
        assert all(payload['type']=='GetSnapshot' for kind,payload in operation_requests if kind=='power')
        print('PASS: power rejection is actionable, unavailable actions stay disabled, reconnect restores status, and no power action was dispatched')
        raise SystemExit(0)
    if os.environ.get('FOCALDESK_SETTINGS_TEST_SCOPE') == 'wallpapers':
        catalog = invoke('get_appearance', {})
        collection = [theme for theme in catalog['themes'] if theme['id'].startswith('package:')]
        assert len(collection) == 7
        for theme in collection:
            before = invoke('get_appearance', {})['appearance']
            result = invoke('apply_appearance', {'expected':before,'requested':dict(before,theme=theme['id'])})
            document_path = Path(result['appearance']['theme'])
            document = tomllib.loads(document_path.read_text())
            wallpaper = Path(document['wallpaper']['path'])
            assert wallpaper.is_relative_to(document_path.parent) and wallpaper.is_file() and wallpaper.stat().st_size>0, theme
        assert json.loads(settings.read_text())['fixture_unrelated']=='preserved'
        print('PASS: every collection Apply installs a real portable wallpaper and preserves unrelated preferences')
        raise SystemExit(0)
    if os.environ.get('FOCALDESK_SETTINGS_TEST_SCOPE') == 'connection':
        js("document.querySelector('nav button[aria-label=Displays]').click()")
        wait(lambda:'root-owned installed preview' in text(), 'closed compositor connection explanation')
        assert 'No active displays are available' not in text()
        assert 'Display information could not be loaded' in text()
        deny_displays = False
        wait(lambda:'DP-1' in text(), 'automatic connection recovery')
        wait(lambda:'root-owned installed preview' not in text(), 'connection error clears after successful poll')
        assert 'Display information could not be loaded' not in text()
        print('PASS: closed compositor connection explains executable trust, never claims an absent session, and clears automatically after reconnect')
        raise SystemExit(0)
    if os.environ.get('FOCALDESK_SETTINGS_TEST_SCOPE') == 'exclusive':
        expected = invoke('get_exclusive_hdr', {})
        assert expected['phase'] == 'off'
        exclusive_file = ROOT/'state/focaldesk/exclusive-hdr.json'
        assert 'disconnected' in invoke('restart_exclusive_hdr', {'connector':'missing','expected':expected,'enabled':True},ok=False)
        assert not exclusive_file.exists()
        display_state['hardware_modes'] = False
        assert 'physical DRM' in invoke('restart_exclusive_hdr', {'connector':'DP-1','expected':expected,'enabled':True},ok=False)
        display_state['hardware_modes'] = True
        configs = [output['config'] for output in display_state['outputs']]
        trial = invoke('preview_displays', {'expected':configs,'outputs':configs})['preview']
        assert 'Finish display trials' in invoke('restart_exclusive_hdr', {'connector':'DP-1','expected':expected,'enabled':True},ok=False)
        invoke('revert_displays', {'previewId':trial['id']})
        assert not exclusive_file.exists()
        result = invoke('restart_exclusive_hdr', {'connector':'DP-1','expected':expected,'enabled':True})
        assert 'logout requested' in result
        armed = invoke('get_exclusive_hdr', {})
        assert armed['phase']=='requested' and armed['connector']=='DP-1'
        assert 'changed' in invoke('restart_exclusive_hdr', {'connector':'DP-1','expected':expected,'enabled':False},ok=False)
        logout_reject = True
        result = invoke('restart_exclusive_hdr', {'connector':'DP-1','expected':armed,'enabled':False})
        assert 'Log out manually' in result
        disabled = invoke('get_exclusive_hdr', {})
        assert disabled['phase']=='disabled' and disabled['reason']
        assert settings.read_text()==original
        assert len([row for row in requests if row['type']=='ExecuteDesktopAction'])==2
        print('PASS: exclusive HDR validation, active-trial guard, stale state, acknowledged logout, rejected logout/manual activation, and canonical-settings preservation')
        raise SystemExit(0)
    assert js("return document.querySelectorAll('.theme-choice').length") >= 6
    (ROOT / "appearance.png").write_bytes(base64.b64decode(wd("GET", "/screenshot")))
    assert js("const e=Array.from(document.querySelectorAll('.theme-choice')).find(e=>e.querySelector('.theme-name').textContent==='Classic');e.click();return true")
    wait(lambda: "You have unapplied changes" in text(), "local preview")
    assert settings.read_text() == original
    assert not requests
    assert not (ROOT / "data/focaldesk/themes").exists()
    button("Discard")
    assert not requests
    js("const e=document.getElementById('font-scale');e.value='1.25';e.dispatchEvent(new Event('input',{bubbles:true}))")
    button("Apply changes")
    wait(lambda: "Your appearance settings are applied" in text(), "apply acknowledgement")
    document = json.loads(settings.read_text())
    assert document["desktop_config"]["appearance"]["font_scale"] == 1.25
    assert document["desktop_config"]["dock"]["visibility"] == "autohide"
    assert document["fixture_unrelated"] == "preserved"
    # Surface controls remain draft-only and preserve nullable theme inheritance.
    before_surfaces = settings.read_bytes()
    js("document.getElementById('shader-chrome').click();document.getElementById('work-area-glass').click();document.getElementById('panel-darkness-default').click()")
    wait(lambda:js("return !!document.getElementById('panel-darkness')"),'explicit panel darkness control')
    js("const e=document.getElementById('panel-darkness');e.value='0.62';e.dispatchEvent(new Event('input',{bubbles:true}));for(const [id,value] of [['rail-backdrop','smoked-glass'],['task-shelf-visibility','always-visible']]){const e=document.getElementById(id);e.value=value;e.dispatchEvent(new Event('change',{bubbles:true}));}")
    assert settings.read_bytes() == before_surfaces
    button('Discard')
    assert js("return document.getElementById('panel-darkness-default').checked && !document.getElementById('panel-darkness') && document.getElementById('shader-chrome').checked")
    js("document.getElementById('shader-chrome').click();document.getElementById('work-area-glass').click();document.getElementById('panel-darkness-default').click()")
    wait(lambda:js("return !!document.getElementById('panel-darkness')"),'panel darkness draft ready')
    js("const e=document.getElementById('panel-darkness');e.value='0.62';e.dispatchEvent(new Event('input',{bubbles:true}));for(const [id,value] of [['rail-backdrop','smoked-glass'],['task-shelf-visibility','always-visible']]){const e=document.getElementById(id);e.value=value;e.dispatchEvent(new Event('change',{bubbles:true}));}")
    button('Apply changes')
    wait(lambda:json.loads(settings.read_text())['desktop_config']['appearance'].get('panel_darkness')==0.62,'surface settings saved')
    wait(lambda:'Your appearance settings are applied' in text(),'surface apply acknowledgement')
    surfaces = json.loads(settings.read_text())['desktop_config']
    assert surfaces['appearance']['shader_chrome'] is False and surfaces['appearance']['work_area_glass'] is False
    assert surfaces['appearance']['rail_backdrop']=='smoked-glass' and surfaces['dock']['visibility']=='always-visible'
    (ROOT/'appearance-surfaces.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    js("document.getElementById('panel-darkness-default').click()")
    button('Apply changes')
    wait(lambda:json.loads(settings.read_text())['desktop_config']['appearance'].get('panel_darkness') is None,'theme panel darkness inheritance saved')
    wait(lambda:'Your appearance settings are applied' in text(),'theme inheritance acknowledgement')
    current_appearance = invoke('get_appearance',{})['appearance']
    invoke('apply_appearance',{'expected':current_appearance,'requested':dict(current_appearance,panel_darkness=1.01)},ok=False)
    invoke('apply_appearance',{'expected':current_appearance,'requested':dict(current_appearance,rail_backdrop='invalid')},ok=False)
    # A shelf edit from another client is part of appearance's stale-edit check.
    concurrent = json.loads(settings.read_text())
    concurrent['desktop_config']['dock']['visibility']='autohide'
    settings.write_text(json.dumps(concurrent))
    before_conflict = settings.read_bytes()
    invoke('apply_appearance',{'expected':current_appearance,'requested':dict(current_appearance,shader_chrome=True)},ok=False)
    assert settings.read_bytes()==before_conflict
    invoke('apply_appearance',{'expected':invoke('get_appearance',{})['appearance'],'requested':current_appearance})
    js("document.getElementById('focus-glow').click()")
    reject = True
    button("Apply changes")
    wait(lambda: "Fixture rejected appearance" in text(), "backend rejection")
    assert json.loads(settings.read_text())["desktop_config"]["appearance"]["output_focus_glow"] is True
    assert "You have unapplied changes" in text()
    reject = False
    button("Discard")
    js("const e=Array.from(document.querySelectorAll('.theme-choice')).find(e=>e.querySelector('.theme-name').textContent==='Classic');e.click()")
    button("Apply changes")
    wait(lambda: "Your appearance settings are applied" in text(), "theme install and apply")
    assert (ROOT / "data/focaldesk/themes/classic/theme.toml").is_file()
    js("document.getElementById('focus-glow').click()")
    document = json.loads(settings.read_text())
    document["desktop_config"]["appearance"]["font_scale"] = 1.5
    settings.write_text(json.dumps(document))
    count = len(requests)
    button("Apply changes")
    wait(lambda: "Appearance changed in another window" in text(), "stale edit rejection")
    assert len(requests) == count
    js("Array.from(document.querySelectorAll('nav button')).find(e=>e.textContent.trim()==='▣Displays').click()")
    wait(lambda: "Your workspace, connected" in text(), "display page")
    wait(lambda: js("return !!document.getElementById('display-resolution')"), "display controls")
    (ROOT / "displays.png").write_bytes(base64.b64decode(wd("GET", "/screenshot")))
    before_displays = settings.read_text()
    wait(lambda:js("return !!document.getElementById('layout-topbar_on_all_outputs')"),'native display layout controls')
    layout_original=invoke('get_display_layout_preferences',{})
    js("document.getElementById('layout-topbar_on_all_outputs').click()")
    button('Discard layout preferences')
    assert settings.read_text()==before_displays
    js("for(const key of ['topbar_on_all_outputs','sidebar_on_all_outputs','remember_focused_output'])document.getElementById('layout-'+key).click()")
    button('Apply layout preferences')
    wait(lambda:'Display layout preferences applied' in text(),'layout preference acknowledgement')
    layout_saved=json.loads(settings.read_text())
    assert all(layout_saved['desktop_config']['displays'][key] is not value for key,value in layout_original.items())
    original_document=json.loads(before_displays)
    assert layout_saved['desktop_config']['appearance']==original_document['desktop_config']['appearance']
    assert layout_saved['desktop_config']['dock']==original_document['desktop_config']['dock']
    js("document.getElementById('layout-topbar_on_all_outputs').click()")
    reject=True
    layout_bytes=settings.read_bytes()
    button('Apply layout preferences')
    wait(lambda:'Fixture rejected appearance' in text(),'layout compositor rejection')
    assert settings.read_bytes()==layout_bytes
    reject=False
    button('Discard layout preferences')
    old_layout=invoke('get_display_layout_preferences',{})
    concurrent=json.loads(settings.read_text())
    concurrent['desktop_config']['displays']['remember_focused_output']=not old_layout['remember_focused_output']
    settings.write_text(json.dumps(concurrent))
    before_layout_conflict=settings.read_bytes()
    invoke('apply_display_layout_preferences',{'expected':old_layout,'requested':dict(old_layout,topbar_on_all_outputs=not old_layout['topbar_on_all_outputs'])},ok=False)
    assert settings.read_bytes()==before_layout_conflict
    button('Reload layout preferences')
    wait(lambda:js("return !Array.from(document.querySelectorAll('button')).find(e=>e.textContent==='Reload layout preferences').disabled"),'layout reload finished')
    before_displays=settings.read_text()
    js("const e=document.getElementById('display-refresh');e.value='120000';e.dispatchEvent(new Event('change',{bubbles:true}))")
    js("const e=document.getElementById('display-scale');e.value='1.25';e.dispatchEvent(new Event('change',{bubbles:true}))")
    js("const e=document.getElementById('display-orientation');e.value='Rotate90';e.dispatchEvent(new Event('change',{bubbles:true}))")
    js("const e=document.getElementById('display-color');e.value='display_p3';e.dispatchEvent(new Event('change',{bubbles:true}))")
    js("document.getElementById('display-hdr').click()")
    assert settings.read_text() == before_displays
    assert not any(request["type"] == "PreviewDisplays" for request in requests)
    button("Test changes")
    wait(lambda: "Keep this display setup?" in text(), "timed display confirmation")
    trial = next(request for request in reversed(requests) if request["type"] == "PreviewDisplays")
    assert trial["outputs"][0]["refresh_mhz"] == 120000
    assert trial["outputs"][0]["scale"] == 1.25
    assert trial["outputs"][0]["transform"] == "Rotate90"
    assert trial["outputs"][0]["color_profile"] == "display_p3"
    assert trial["outputs"][0]["hdr_requested"] is True
    assert settings.read_text() == before_displays
    (ROOT / "display-confirmation.png").write_bytes(base64.b64decode(wd("GET", "/screenshot")))
    button("Revert now")
    wait(lambda: "Keep this display setup?" not in text(), "explicit display revert")
    assert settings.read_text() == before_displays
    js("const e=document.getElementById('display-resolution');e.value='2560×1440';e.dispatchEvent(new Event('change',{bubbles:true}))")
    js("const e=document.getElementById('display-scale');e.value='1.25';e.dispatchEvent(new Event('change',{bubbles:true}))")
    button("Test changes")
    wait(lambda: "Keep this display setup?" in text(), "second display trial")
    button("Keep changes")
    wait(lambda: "Your display settings are saved" in text(), "display confirmation persisted")
    document = json.loads(settings.read_text())
    assert document["displays"]["outputs"][0]["width"] == 2560
    assert document["displays"]["outputs"][0]["scale"] == 1.25
    assert document["desktop_config"]["appearance"]["font_scale"] == 1.5
    assert document["fixture_unrelated"] == "preserved"
    # Real Rust audio commands execute private pactl/parec fixtures, never host audio.
    # A connected disabled connector remains selectable for a timed re-enable.
    secondary=copy.deepcopy(display_state['outputs'][0])
    secondary['config'].update(connector='DP-2',enabled=False,primary=False,x=2048,role='Normal')
    display_state['outputs'].append(secondary)
    wait(lambda:js("return Array.from(document.querySelectorAll('.monitor-tabs button span')).some(e=>e.textContent==='DP-2')"),'disabled connector remains in inventory')
    js("Array.from(document.querySelectorAll('.monitor-tabs button span')).find(e=>e.textContent==='DP-2').parentElement.click()")
    wait(lambda:js("return !document.getElementById('display-enabled').checked"),'disabled display selected')
    js("document.getElementById('display-enabled').click();const e=document.getElementById('display-role');e.value='ControlPanel';e.dispatchEvent(new Event('change',{bubbles:true}))")
    button('Test changes')
    wait(lambda:'Keep this display setup?' in text(),'role and re-enable trial')
    button('Keep changes')
    wait(lambda:'Your display settings are saved' in text(),'role and re-enable confirmed')
    assert display_state['outputs'][1]['config']['enabled'] is True
    assert display_state['outputs'][1]['config']['role']=='ControlPanel'
    js("document.getElementById('display-enabled').click()")
    button('Test changes')
    wait(lambda:'Keep this display setup?' in text(),'disable trial')
    button('Revert now')
    wait(lambda:js("return document.getElementById('display-enabled').checked && !document.querySelector('.display-confirm')"),'disabled connector restored by revert')
    js("document.getElementById('display-enabled').click()")
    button('Test changes')
    wait(lambda:'Keep this display setup?' in text(),'second disable trial')
    button('Keep changes')
    wait(lambda:'Your display settings are saved' in text(),'disable confirmed')
    assert json.loads(settings.read_text())['displays']['outputs'][1]['enabled'] is False
    js("document.getElementById('display-enabled').click();const e=document.getElementById('display-role');e.value='Normal';e.dispatchEvent(new Event('change',{bubbles:true}))")
    button('Test changes')
    wait(lambda:'Keep this display setup?' in text(),'restore desktop role trial')
    button('Keep changes')
    wait(lambda:'Your display settings are saved' in text(),'desktop role confirmed')
    assert display_state['outputs'][0]['config']['primary'] is True
    assert display_state['outputs'][1]['config']['role']=='Normal'
    js("document.getElementById('selected-display-title').scrollIntoView()")
    (ROOT/'display-enable-role.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    # HDR appearance and ICC stay in the same draft/trial; patterns are session-only.
    private_icc=ROOT/'private-display.icc'
    write_srgb(private_icc)
    before_color=settings.read_bytes()
    js("document.getElementById('display-hdr').click()")
    wait(lambda:js("return !document.getElementById('hdr-preset').disabled && document.getElementById('hdr-preset').options.length===4"),'HDR presets loaded')
    js("const preset=document.getElementById('hdr-preset');preset.value='2';preset.dispatchEvent(new Event('change',{bubbles:true}));const p=document.getElementById('hdr-peak_nits');p.value='800';p.dispatchEvent(new Event('input',{bubbles:true}));const f=document.getElementById('hdr-full_frame_peak_nits');f.value='500';f.dispatchEvent(new Event('input',{bubbles:true}));const t=document.getElementById('hdr-tone-mapper');t.value='aces';t.dispatchEvent(new Event('change',{bubbles:true}))")
    js('const e=document.getElementById("display-icc");e.value='+json.dumps(str(private_icc))+';e.dispatchEvent(new Event("input",{bubbles:true}))')
    assert settings.read_bytes()==before_color
    button('Test changes')
    wait(lambda:'Keep this display setup?' in text(),'ICC/HDR timed trial')
    wait(lambda:js("return !document.getElementById('hdr-pattern').disabled"),'trial pattern controls active')
    js("const e=document.getElementById('hdr-pattern');e.value='overview';e.dispatchEvent(new Event('change',{bubbles:true}))")
    wait(lambda:display_patterns.get('DP-2')=='overview','owned calibration pattern selected')
    assert settings.read_bytes()==before_color
    button('Keep changes')
    wait(lambda:'Your display settings are saved' in text(),'ICC/HDR keep')
    saved_color=json.loads(settings.read_text())['displays']['outputs'][1]
    assert saved_color['icc_profile_path']==str(private_icc)
    assert saved_color['hdr_appearance']['peak_nits']==800 and saved_color['hdr_appearance']['full_frame_peak_nits']==500
    assert saved_color['hdr_appearance']['tone_mapper']=='aces'
    assert not display_patterns
    assert js("return document.getElementById('hdr-pattern').disabled")
    button('Clear ICC')
    button('Test changes')
    wait(lambda:'Keep this display setup?' in text(),'ICC clear trial')
    button('Keep changes')
    wait(lambda:'Your display settings are saved' in text(),'ICC clear saved')
    assert json.loads(settings.read_text())['displays']['outputs'][1]['icc_profile_path'] is None
    js("document.getElementById('hdr-peak_nits').scrollIntoView()")
    (ROOT/'display-hdr-calibration.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    js("Array.from(document.querySelectorAll('nav button')).find(e=>e.textContent.trim()==='♫Sound').click()")
    wait(lambda: 'PipeWire fixture' in text(), 'Sound device inventory')
    assert js("return document.querySelectorAll('#sound-input option').length") == 3
    assert 'Monitor of Studio' not in text()
    assert not (audio_root / 'capture.jsonl').exists()
    (ROOT / 'sound.png').write_bytes(base64.b64decode(wd('GET', '/screenshot')))
    js("const e=document.getElementById('sound-output');e.value='output:8:headphones.output';e.dispatchEvent(new Event('change',{bubbles:true}))")
    wait(lambda: json.loads((audio_root / 'audio.json').read_text())['default_output'] == 'headphones.output', 'default output')
    wait(lambda: not js("return document.getElementById('sound-output-volume').disabled"), 'output controls ready')
    js("const e=document.getElementById('sound-output-volume');e.value='60';e.dispatchEvent(new Event('input',{bubbles:true}));e.dispatchEvent(new Event('change',{bubbles:true}))")
    wait(lambda: json.loads((audio_root / 'audio.json').read_text())['outputs'][1]['volume']['mono']['value'] == round(.60 * 65536), 'output volume')
    wait(lambda: not js("return document.getElementById('sound-output-mute').disabled"), 'output mute ready')
    js("document.getElementById('sound-output-mute').click()")
    wait(lambda: json.loads((audio_root / 'audio.json').read_text())['outputs'][1]['mute'], 'output mute')
    wait(lambda: not js("return document.getElementById('sound-output-mute').disabled"), 'unmute ready')
    js("document.getElementById('sound-output-mute').click()")
    wait(lambda: not json.loads((audio_root / 'audio.json').read_text())['outputs'][1]['mute'], 'output unmute')
    wait(lambda: not js("return document.getElementById('sound-input-volume').disabled"), 'input controls ready')
    js("const e=document.getElementById('sound-input-volume');e.value='55';e.dispatchEvent(new Event('input',{bubbles:true}));e.dispatchEvent(new Event('change',{bubbles:true}))")
    wait(lambda: json.loads((audio_root / 'audio.json').read_text())['inputs'][0]['volume']['mono']['value'] == round(.55 * 65536), 'input volume')
    wait(lambda: not js("return document.getElementById('sound-input-mute').disabled"), 'input mute ready')
    js("document.getElementById('sound-input-mute').click()")
    wait(lambda: json.loads((audio_root / 'audio.json').read_text())['inputs'][0]['mute'], 'input mute')
    wait(lambda: js("return Array.from(document.querySelectorAll('button')).find(e=>e.textContent==='Test microphone').disabled"), 'muted test blocked')
    wait(lambda: not js("return document.getElementById('sound-input-mute').disabled"), 'input unmute ready')
    js("document.getElementById('sound-input-mute').click()")
    wait(lambda: not json.loads((audio_root / 'audio.json').read_text())['inputs'][0]['mute'], 'input unmute')
    wait(lambda: not js("return document.getElementById('sound-output-mute').disabled"), 'error control ready')
    (audio_root / 'reject').touch()
    js("document.getElementById('sound-output-mute').click()")
    wait(lambda: 'Fixture rejected audio change' in text(), 'audio rejection shown')
    assert not json.loads((audio_root / 'audio.json').read_text())['outputs'][1]['mute']
    assert not js("return document.getElementById('sound-output-mute').checked")
    (audio_root / 'reject').unlink()
    button('Refresh devices')
    wait(lambda: 'Fixture rejected audio change' not in text(), 'audio refresh clears error')
    js("const e=document.getElementById('sound-input');e.value='input:4:webcam.mic';e.dispatchEvent(new Event('change',{bubbles:true}))")
    wait(lambda: json.loads((audio_root / 'audio.json').read_text())['default_input'] == 'webcam.mic', 'default microphone')
    wait(lambda: not js("return document.getElementById('sound-input').disabled"), 'input selection ready')
    button('Test microphone')
    wait(lambda: js("return document.getElementById('microphone-level').value > 0"), 'selected microphone level')
    selected_capture = json.loads((audio_root / 'capture.jsonl').read_text().splitlines()[-1])
    assert '--device=webcam.mic' in selected_capture['args']
    js("const e=document.getElementById('sound-input');e.value='input:3:usb.mic';e.dispatchEvent(new Event('change',{bubbles:true}))")
    wait(lambda: not Path(f"/proc/{selected_capture['pid']}").exists(), 'device change stops previous microphone test')
    wait(lambda: json.loads((audio_root / 'audio.json').read_text())['default_input'] == 'usb.mic', 'restore default microphone')
    wait(lambda: not js("return document.getElementById('sound-input').disabled"), 'restored microphone controls ready')
    button('Test microphone')
    wait(lambda: js("return document.getElementById('microphone-level').value > 0"), 'synthetic microphone level')
    (ROOT / 'sound-microphone.png').write_bytes(base64.b64decode(wd('GET', '/screenshot')))
    capture = json.loads((audio_root / 'capture.jsonl').read_text().splitlines()[-1])
    assert '--device=usb.mic' in capture['args']
    assert '--property=node.dont-reconnect=true' in capture['args']
    external = json.loads((audio_root / 'audio.json').read_text())
    external['inputs'][0]['mute'] = True
    temporary = audio_root / 'external.tmp'
    temporary.write_text(json.dumps(external)); temporary.replace(audio_root / 'audio.json')
    wait(lambda: not Path(f"/proc/{capture['pid']}").exists(), 'external microphone mute stops capture')
    wait(lambda: js("return document.getElementById('sound-input-mute').checked"), 'external mute reflected')
    js("document.getElementById('sound-input-mute').click()")
    wait(lambda: not json.loads((audio_root / 'audio.json').read_text())['inputs'][0]['mute'], 'restore external mute')
    wait(lambda: not js("return document.getElementById('sound-input-mute').disabled"), 'restored microphone ready')
    button('Test microphone')
    wait(lambda: js("return document.getElementById('microphone-level').value > 0"), 'explicit stop test started')
    capture = json.loads((audio_root / 'capture.jsonl').read_text().splitlines()[-1])
    button('Stop test')
    wait(lambda: not Path(f"/proc/{capture['pid']}").exists(), 'explicit microphone stop')
    button('Test microphone')
    wait(lambda: js("return document.getElementById('microphone-level').value > 0"), 'second microphone level')
    capture = json.loads((audio_root / 'capture.jsonl').read_text().splitlines()[-1])
    js("window.dispatchEvent(new Event('blur'))")
    wait(lambda: not Path(f"/proc/{capture['pid']}").exists(), 'blur microphone stop')
    wait(lambda: 'Not listening' in text(), 'blur status')
    button('Test microphone')
    wait(lambda: js("return document.getElementById('microphone-level').value > 0"), 'third microphone level')
    capture = json.loads((audio_root / 'capture.jsonl').read_text().splitlines()[-1])
    js("document.querySelector('nav button[aria-label=About]').click()")
    wait(lambda: not Path(f"/proc/{capture['pid']}").exists(), 'page exit microphone stop')
    js("Array.from(document.querySelectorAll('nav button')).find(e=>e.textContent.includes('Keyboard')).click()")
    wait(lambda: js("return !!document.getElementById('input-layout')"), 'native input page')
    before_input = settings.read_text()
    js("const e=document.getElementById('input-try-typing');e.value='private typing sample';e.dispatchEvent(new Event('input',{bubbles:true}))")
    assert settings.read_text() == before_input
    js("const e=document.getElementById('input-layout');e.value='de';e.dispatchEvent(new Event('change',{bubbles:true}))")
    button('Discard')
    assert js("return document.getElementById('input-layout').value") == 'us'
    js("const e=document.getElementById('input-repeat-rate');e.value='40';e.dispatchEvent(new Event('input',{bubbles:true}))")
    button('Apply changes')
    wait(lambda: 'Keyboard and input settings are applied.' in text(), 'input apply through Rust IPC')
    assert input_state['keyboard_repeat_rate'] == 40
    assert json.loads(settings.read_text())['fixture_unrelated'] == 'preserved'
    assert 'private typing sample' not in settings.read_text()
    button('Mouse & touchpad')
    wait(lambda: 'Workspace gestures' in text(), 'pointer and gesture controls')
    before_reset = settings.read_bytes()
    js("const e=document.getElementById('input-pointer-speed');if(e){e.value='1.5';e.dispatchEvent(new Event('input',{bubbles:true}));}")
    button('Reset pointer and gestures')
    assert settings.read_bytes() == before_reset
    button('Shortcuts')
    wait(lambda: js("return !!document.getElementById('shortcut-launch_terminal')"), 'shortcut editor')
    aria_button('Capture shortcut for Launch terminal')
    js("document.getElementById('shortcut-launch_terminal').dispatchEvent(new KeyboardEvent('keydown',{key:'F9',ctrlKey:true,bubbles:true,cancelable:true}))")
    wait(lambda:js("return document.getElementById('shortcut-launch_terminal').value==='Ctrl+F9'"),'local shortcut capture')
    assert settings.read_bytes() == before_reset
    button('Discard')

    (ROOT / 'input.png').write_bytes(base64.b64decode(wd('GET', '/screenshot')))
    # Keyboard and pointer controls share one sidebar destination.
    assert js("return document.querySelectorAll('nav button').length") == 17
    legacy_labels = js("return Array.from(document.querySelectorAll('nav button')).filter(e=>e.title.includes('opens existing Settings')).map(e=>e.title.split(' — ')[0])")
    assert legacy_labels == []
    js("document.querySelector('nav button[aria-label=\"Keyboard & Input\"]').click()")
    button('Mouse & touchpad')
    wait(lambda: 'Workspace gestures' in text(), 'direct mouse and touchpad route')
    js("document.querySelector('nav button[aria-label=Privacy]').click()")
    wait(lambda: js("return !!document.getElementById('privacy-recent_files')"), 'native privacy page')
    permission_file = ROOT / 'config/focaldesk/ai_permissions.toml'
    permission_file.write_text('[[entries]]\napp_identity = "org.focaldesk.Fixture"\nresource = "AiChat"\ntarget = "global"\ndecision = "Allow"\nscope = "Persistent"\nupdated_at_unix = 1\n')
    permission_rows = invoke('get_saved_permissions', {})['records']
    record = next(row for row in permission_rows if row['app']=='org.focaldesk.Fixture')
    permission_file.write_text(permission_file.read_text().replace('"Allow"','"Deny"'))
    assert 'changed' in invoke('revoke_saved_permission', {'id':record['id']}, ok=False)
    record = next(row for row in invoke('get_saved_permissions', {})['records'] if row['app']=='org.focaldesk.Fixture')
    invoke('revoke_saved_permission', {'id':record['id']})
    assert not any(row['app']=='org.focaldesk.Fixture' for row in invoke('get_saved_permissions', {})['records'])
    privacy_before = json.loads(settings.read_text())
    js("document.getElementById('privacy-recent_files').click()")
    button('Discard')
    assert json.loads(settings.read_text()) == privacy_before
    js("document.getElementById('privacy-recent_files').click()")
    button('Apply changes')
    wait(lambda: 'Preferences saved' in text(), 'privacy save')
    operation_reject = True
    pref = invoke('get_preferences', {})['privacy']
    failed_live = invoke('apply_preferences', {'change':{'section':'privacy','expected':pref,'requested':pref}})
    assert 'live notification retention failed' in failed_live['message']
    operation_reject = False
    retried = invoke('apply_preferences', {'change':{'section':'privacy','expected':pref,'requested':pref}})
    assert 'live notification retention failed' not in retried['message']
    privacy_after = json.loads(settings.read_text())
    assert privacy_after['privacy']['recent_files'] is False
    assert privacy_after['input'] == privacy_before['input']
    assert privacy_after['displays'] == privacy_before['displays']
    # An external edit must not be overwritten by an older draft.
    external = copy.deepcopy(privacy_after)
    external['privacy']['location_services'] = not external['privacy']['location_services']
    settings.write_text(json.dumps(external))
    js("document.getElementById('privacy-recent_files').click()")
    button('Apply changes')
    wait(lambda: 'changed in another client' in text(), 'privacy stale edit rejection')
    assert json.loads(settings.read_text()) == external
    button('Reload')
    wait(lambda: not js("return document.querySelector('fieldset').disabled"), 'privacy reload')
    (ROOT / 'privacy.png').write_bytes(base64.b64decode(wd('GET', '/screenshot')))
    js("document.querySelector('nav button[aria-label=Power]').click()")
    wait(lambda: js("return !!document.getElementById('power-blank')"), 'native power page')
    js("const e=document.getElementById('power-blank');e.value=Array.from(e.options).find(o=>o.textContent==='5 minutes').value;e.dispatchEvent(new Event('change',{bubbles:true}))")
    button('Apply changes')
    wait(lambda: 'Preferences saved' in text(), 'power save')
    power_after = json.loads(settings.read_text())
    assert power_after['power']['blank_screen_minutes'] == 5
    assert power_after['privacy'] == external['privacy']
    assert power_after['input'] == external['input']
    wait(lambda: 'Fixture battery' in text() and '73%' in text(), 'live power snapshot')
    button('Restart…')
    assert not any(payload['type']=='Reboot' for kind,payload in operation_requests)
    button('Cancel')
    button('Restart…'); button('Confirm Restart')
    wait(lambda:'Power action queued' in text(),'truthful queued power action')
    assert any(payload['type']=='Reboot' for kind,payload in operation_requests)
    operation_reject = True
    button('Suspend…'); button('Confirm Suspend')
    wait(lambda:'Fixture authorization rejected' in text(),'power rejection')
    operation_reject = False
    assert any(kind=='notification' and payload['type']=='SetHistoryLimit' for kind,payload in operation_requests)
    (ROOT / 'power.png').write_bytes(base64.b64decode(wd('GET', '/screenshot')))
    # Accessibility uses one Rust transaction for visual and keyboard preferences.
    accessibility_before = json.loads(settings.read_text())
    accessibility_before['accessibility'] = {'future_visual': 42, 'night_light': {'future_light': 17}, 'magnifier_output_zoom': {'DP-1': 3.0}}
    accessibility_before['input']['accessibility']['future_keyboard'] = 23
    settings.write_text(json.dumps(accessibility_before))
    js("document.querySelector('nav button[aria-label=Accessibility]').click()")
    wait(lambda: js("return !!document.getElementById('accessibility-sticky_keys')"), 'native accessibility page')
    assert js("return document.getElementById('accessibility-slow-delay').disabled")
    assert js("return document.getElementById('accessibility-temperature').disabled")
    js("document.getElementById('accessibility-sticky_keys').focus()")
    wd('POST','/actions',{'actions':[{'type':'key','id':'keyboard','actions':[{'type':'keyDown','value':' '},{'type':'keyUp','value':' '}]}]})
    wait(lambda: js("return document.getElementById('accessibility-sticky_keys').checked"), 'keyboard operated accessibility switch')
    button('Discard')
    assert json.loads(settings.read_text()) == accessibility_before
    js("document.getElementById('accessibility-sticky_keys').click();document.getElementById('accessibility-slow_keys').click();document.getElementById('accessibility-magnifier').click();document.getElementById('accessibility-night-light').click()")
    assert not js("return document.getElementById('accessibility-slow-delay').disabled")
    js("const e=document.getElementById('accessibility-zoom');e.value='2.5';e.dispatchEvent(new Event('input',{bubbles:true}));const c=document.getElementById('accessibility-cursor');c.value='48';c.dispatchEvent(new Event('input',{bubbles:true}))")
    js("const e=document.getElementById('accessibility-schedule');e.value='automatic';e.dispatchEvent(new Event('change',{bubbles:true}))")
    wait(lambda: js("return !!document.getElementById('accessibility-latitude')"), 'solar scheduling controls')
    js("for(const [id,value] of [['accessibility-latitude','51.5'],['accessibility-longitude','-0.12'],['accessibility-start','20:30']]){const e=document.getElementById(id);e.value=value;e.dispatchEvent(new Event('input',{bubbles:true}))}")
    js("document.getElementById('comfort-DP-1').click()")
    # A different input edit after loading must survive this narrower save.
    concurrent = json.loads(settings.read_text())
    concurrent['input']['keyboard_layout'] = 'de'
    settings.write_text(json.dumps(concurrent))
    button('Apply changes')
    wait(lambda: 'Preferences saved' in text(), 'accessibility atomic save')
    accessibility_after = json.loads(settings.read_text())
    assert accessibility_after['accessibility']['cursor_size'] == 48
    assert accessibility_after['accessibility']['magnifier_output_zoom'] == {}
    assert accessibility_after['accessibility']['night_light']['custom_start_minutes'] == 1230
    assert accessibility_after['accessibility']['night_light']['latitude'] == 51.5
    assert accessibility_after['accessibility']['display_comfort_disabled_outputs'] == {'DP-1': True}
    assert accessibility_after['accessibility']['future_visual'] == 42
    assert accessibility_after['accessibility']['night_light']['future_light'] == 17
    assert accessibility_after['input']['accessibility']['future_keyboard'] == 23
    assert accessibility_after['input']['accessibility']['sticky_keys'] is True
    assert accessibility_after['input']['keyboard_layout'] == 'de'
    assert accessibility_after['input']['keybindings'] == concurrent['input']['keybindings']
    for section in ['privacy','power','displays','desktop_config']:
        assert accessibility_after[section] == concurrent[section]
    # Reset removes obsolete overrides but stays a draft until Apply.
    button('Reset color comfort')
    button('Apply changes')
    wait(lambda: 'Preferences saved' in text(), 'neutral color reset')
    neutral = json.loads(settings.read_text())
    assert neutral['accessibility']['night_light']['enabled'] is False
    assert neutral['accessibility']['display_comfort_disabled_outputs'] == {}
    js("document.getElementById('accessibility-sticky_keys').click()")
    external_accessibility = copy.deepcopy(neutral)
    external_accessibility['accessibility']['cursor_size'] = 56
    settings.write_text(json.dumps(external_accessibility))
    button('Apply changes')
    wait(lambda: 'Accessibility preferences changed in another client' in text(), 'accessibility stale rejection')
    assert json.loads(settings.read_text()) == external_accessibility
    (ROOT / 'accessibility.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    # Real Rust processes use private NetworkManager / BlueZ fixtures.
    js("document.querySelector('nav button[aria-label=Network]').click()")
    wait(lambda: 'Office: West' in text(), 'NetworkManager escaped SSID inventory')
    state = lambda: json.loads((connectivity_root/'state.json').read_text())
    assert state()['scans'] == 0
    button('Scan for networks')
    wait(lambda: state()['scans']==1, 'explicit Wi-Fi rescan')
    wait(lambda: js("return !document.querySelector('button[aria-label=\"Connect to Office: West on wlan0\"]').disabled"), 'Wi-Fi scan action complete')
    aria_button("Connect to Office: West on wlan0")
    wait(lambda: js("return !!document.getElementById('network-password')"), 'Wi-Fi password form')
    wait(lambda: js("return document.activeElement.id==='network-password'"), 'Wi-Fi password keyboard focus')
    js("const e=document.getElementById('network-password');e.value='incorrect-secret';e.dispatchEvent(new Event('input',{bubbles:true}))")
    button('Connect to network')
    wait(lambda: 'Check the password' in text(), 'Wi-Fi rejected secret')
    assert 'incorrect-secret' not in text()
    assert js("return document.getElementById('network-password').value") == ''
    js("const e=document.getElementById('network-password');e.value='fixture-password';e.dispatchEvent(new Event('input',{bubbles:true}))")
    button('Connect to network')
    wait(lambda: state()['secret_received'], 'Wi-Fi password delivered on stdin')
    wait(lambda: 'Connected · Disconnect' in text(), 'Wi-Fi connected state')
    calls=(connectivity_root/'calls.jsonl').read_text()
    assert 'fixture-password' not in calls and 'incorrect-secret' not in calls
    button('Connected · Disconnect')
    button('Cancel')
    assert state()['wifi_connected'] is not None
    js("document.getElementById('network-wifi').click()")
    button('Confirm network change')
    wait(lambda: not state()['wifi'], 'Wi-Fi radio disable')
    wait(lambda: not js("return document.getElementById('network-wifi').checked"), 'Wi-Fi off reflected')
    js("document.getElementById('network-wifi').click()")
    wait(lambda: state()['wifi'], 'Wi-Fi radio enable')
    wait(lambda: js("return !document.getElementById('network-wifi').disabled"), 'Wi-Fi action complete')
    (ROOT/'network.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    js("document.querySelector('nav button[aria-label=Bluetooth]').click()")
    wait(lambda: 'Studio headphones' in text(), 'BlueZ inventory')
    button('Find devices')
    wait(lambda: 'Stop discovery' in text(), 'Bluetooth discovery started')
    agent = lambda: [json.loads(line) for line in (connectivity_root/'agent.jsonl').read_text().splitlines()]
    wait(lambda: (connectivity_root/'agent.jsonl').exists() and any(row['command']=='scan on' for row in agent()), 'discovery helper input')
    scan_pid=next(row['pid'] for row in agent() if row['command']=='scan on')
    button('Stop discovery')
    wait(lambda: not Path(f'/proc/{scan_pid}').exists(), 'discovery helper cleanup')
    aria_button("Pair Desk keyboard")
    wait(lambda: 'Confirm passkey 123456' in text(), 'Bluetooth passkey prompt')
    wait(lambda: js("return document.activeElement.id==='bluetooth-confirm-code'"), 'Bluetooth comparison keyboard focus')
    assert not any(row['command']=='yes' for row in agent())
    button('Reject code')
    wait(lambda: 'Pairing ended' in text(), 'Bluetooth rejected passkey')
    assert state()['devices'][1]['paired'] is False
    aria_button("Pair Desk keyboard")
    wait(lambda: 'Confirm passkey 123456' in text(), 'Bluetooth second passkey prompt')
    button('Codes match')
    wait(lambda: 'Device paired' in text(), 'Bluetooth confirmed pairing handshake')
    assert state()['devices'][1]['paired'] is True
    aria_button("Connect Desk keyboard")
    wait(lambda: state()['devices'][1]['connected'], 'Bluetooth connect')
    wait(lambda: js("return !!document.querySelector('button[aria-label=\"Disconnect Desk keyboard\"]')"), 'Bluetooth connected row')
    aria_button("Disconnect Desk keyboard")
    button('Confirm Bluetooth change')
    wait(lambda: not state()['devices'][1]['connected'], 'Bluetooth disconnect')
    wait(lambda: js("return !document.querySelector('button[aria-label=\"Forget Desk keyboard\"]').disabled"), 'Bluetooth ready to forget')
    aria_button("Forget Desk keyboard")
    button('Confirm Bluetooth change')
    wait(lambda: not state()['devices'][1]['paired'], 'Bluetooth forget')
    wait(lambda: js("return !!document.querySelector('button[aria-label=\"Pair Desk keyboard\"]')"), 'Bluetooth available after forget')
    (connectivity_root/'pin').touch()
    aria_button("Pair Desk keyboard")
    wait(lambda: 'Enter PIN code' in text(), 'Bluetooth PIN request')
    wait(lambda: js("return document.activeElement.id==='bluetooth-reply'"), 'Bluetooth PIN keyboard focus')
    js("const e=document.getElementById('bluetooth-reply');e.value='1234';e.dispatchEvent(new Event('input',{bubbles:true}))")
    button('Send pairing code')
    wait(lambda: 'Device paired' in text(), 'Bluetooth PIN pairing')
    (ROOT/'bluetooth.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    button('Find devices')
    wait(lambda: 'Stop discovery' in text(), 'second discovery')
    wait(lambda: len([row for row in agent() if row['command']=='scan on'])==2, 'second discovery helper')
    scan_pid=[row['pid'] for row in agent() if row['command']=='scan on'][-1]
    # Even in a short window the final section can be scrolled into view.
    js("document.querySelector('nav button[aria-label=About]').scrollIntoView({block:'nearest'})")
    assert js("const e=document.querySelector('nav button[aria-label=About]').getBoundingClientRect();return e.top>=0 && e.bottom<=innerHeight")
    # Real application persistence and GIO, entirely inside private XDG paths.
    apps = invoke('get_applications', {})['apps']
    rule = dict(app_id='first', title_contains='', workspace=None, output=None, floating=None, maximize=None)
    apps['window_rules'] = [rule, dict(rule, app_id='second')]
    apps['startup_apps'] = [dict(name='Fixture startup', command=str(application_canary), arguments=['--flag', 'path with spaces'], enabled=False)]
    document = json.loads(settings.read_text())
    document['apps'] = copy.deepcopy(apps)
    document['apps']['future'] = 'preserved'
    document['apps']['window_rules'][0]['future_rule'] = 'first metadata'
    document['apps']['window_rules'][1]['future_rule'] = 'second metadata'
    document['apps']['startup_apps'][0]['future_startup'] = True
    settings.write_text(json.dumps(document))
    before_apps = settings.read_bytes()
    js("document.querySelector('nav button[aria-label=Applications]').click()")
    wait(lambda:js("return !!document.getElementById('apps-browser')"),'native applications defaults')
    def input_value(identifier, value):
        js('const e=document.getElementById('+json.dumps(identifier)+');e.value='+json.dumps(value)+';e.dispatchEvent(new Event("input",{bubbles:true}));')
    input_value('apps-browser', str(application_canary))
    button('Discard application changes')
    assert settings.read_bytes() == before_apps
    input_value('apps-browser', str(application_canary))
    button('Window rules')
    aria_button('Move window rule 2 up')
    input_value('rule-workspace-0', '3')
    button('Startup')
    input_value('startup-arg-0-1', 'another path with spaces')
    js("document.getElementById('startup-enabled-0').click()")
    button('Apply application changes')
    wait(lambda:'Save startup changes?' in text(),'startup confirmation')
    assert settings.read_bytes() == before_apps
    button('Save startup changes')
    wait(lambda:'startup entries run at the next sign-in' in text(),'application save acknowledgement')
    actual = json.loads(settings.read_text())
    assert actual['apps']['browser'] == str(application_canary)
    assert actual['apps']['window_rules'][0]['app_id'] == 'second'
    assert actual['apps']['window_rules'][0]['workspace'] == 3
    assert actual['apps']['window_rules'][0]['future_rule'] == 'second metadata'
    assert actual['apps']['window_rules'][1]['future_rule'] == 'first metadata'
    assert actual['apps']['startup_apps'][0]['arguments'] == ['--flag', 'another path with spaces']
    assert actual['apps']['startup_apps'][0]['future_startup'] is True
    assert actual['apps']['future'] == 'preserved'
    assert {k:v for k,v in actual.items() if k!='apps'} == {k:v for k,v in document.items() if k!='apps'}
    (ROOT/'applications-startup.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    button('Defaults')
    input_value('apps-terminal', 'fixture-new-terminal')
    actual['apps']['browser'] = 'external-browser'
    settings.write_text(json.dumps(actual))
    before_stale = settings.read_bytes()
    button('Apply application changes')
    wait(lambda:'changed in another client' in text(),'stale application edit rejection')
    assert settings.read_bytes() == before_stale
    button('Reload applications')
    wait(lambda:js("return document.getElementById('apps-browser').value==='external-browser'"),'application reload')
    button('Reset application settings')
    button('Reset application draft')
    assert settings.read_bytes() == before_stale
    button('Discard application changes')
    current = invoke('get_applications', {})['apps']
    invalid = copy.deepcopy(current)
    invalid['startup_apps'][0]['command'] = ''
    invoke('apply_applications', {'change':dict(expected=current, requested=invalid, rule_origins=[0,1], startup_origins=[0])}, ok=False)
    assert settings.read_bytes() == before_stale
    (ROOT/'applications-defaults.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    button('File associations')
    wait(lambda:js("return !!document.getElementById('association-app') && !document.getElementById('association-app').disabled"),'GIO association registry')
    initial_association = invoke('get_file_association', {'mimeType':'text/plain'})
    assert initial_association['current'] == EDITOR, initial_association
    assert any(app['id']==EDITOR and app['name']=='Fixture Editor' for app in initial_association['applications'])
    js('const e=document.getElementById("association-app");e.value='+json.dumps(OTHER)+';e.dispatchEvent(new Event("change",{bubbles:true}));')
    button('Set default for this type')
    wait(lambda:'Default application updated for this file or link type' in text(),'GIO default association update')
    updated = invoke('get_file_association', {'mimeType':'text/plain'})
    assert updated['current'] == OTHER
    mime_file = ROOT/'config/mimeapps.list'
    assert 'Keep=yes' in mime_file.read_text() and 'application/pdf='+EDITOR+';' in mime_file.read_text()
    mime_file.write_text(mime_file.read_text()+'\n[External Edit]\nKeep=also\n')
    before_mime = mime_file.read_bytes()
    failure = invoke('set_file_association', {'mimeType':'text/plain','expected':updated['current'],'version':updated['version'],'desktopId':EDITOR}, ok=False)
    assert 'changed in another client' in failure
    fresh = invoke('get_file_association', {'mimeType':'text/plain'})
    invoke('set_file_association', {'mimeType':'text/plain','expected':fresh['current'],'version':fresh['version'],'desktopId':'missing.desktop'}, ok=False)
    invoke('get_file_association', {'mimeType':'https://example.com'}, ok=False)
    assert mime_file.read_bytes() == before_mime
    assert settings.read_bytes() == before_stale
    assert not (ROOT/'application-launched').exists()
    (ROOT/'applications-associations.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    # Chrome saves only visibility/order; custom launchers and future keys survive.
    chrome_document = json.loads(settings.read_text())
    chrome_document['chrome'] = {'future':'keep', 'topbar':{'hidden':[9000], 'order':[108,100], 'future':42, 'custom':[{'id':9000,'icon':'terminal','tooltip':'Fixture custom control','command':str(application_canary),'enabled':True,'future':'custom metadata'}]}, 'sidebar':{'hidden':[], 'order':[]}}
    settings.write_text(json.dumps(chrome_document))
    before_chrome = settings.read_bytes()
    js("document.querySelector('nav button[aria-label=Chrome]').click()")
    wait(lambda:js("return !!document.getElementById('chrome-103')"),'native chrome controls')
    assert 'Fixture custom control' in text()
    js("document.getElementById('chrome-103').click()")
    button('Discard chrome changes')
    assert settings.read_bytes() == before_chrome
    assert js("return document.getElementById('chrome-103').checked")
    js("document.getElementById('chrome-103').click();document.getElementById('chrome-1005').click()")
    input_value('chrome-order-topbar', '108, 9000, 100')
    input_value('chrome-order-sidebar', '1007, 1001')
    button('Apply chrome changes')
    wait(lambda:'Preferences saved' in text(),'chrome save acknowledgement')
    saved_chrome = json.loads(settings.read_text())
    assert saved_chrome['chrome']['topbar']['hidden'] == [9000,103]
    assert saved_chrome['chrome']['sidebar']['hidden'] == [1005]
    assert saved_chrome['chrome']['topbar']['order'] == [108,9000,100]
    assert saved_chrome['chrome']['sidebar']['order'] == [1007,1001]
    assert saved_chrome['chrome']['topbar']['custom'] == chrome_document['chrome']['topbar']['custom']
    assert saved_chrome['chrome']['future'] == 'keep' and saved_chrome['chrome']['topbar']['future'] == 42
    assert {k:v for k,v in saved_chrome.items() if k!='chrome'} == {k:v for k,v in chrome_document.items() if k!='chrome'}
    saved_chrome_bytes = settings.read_bytes()
    input_value('chrome-order-topbar', '100, 100')
    button('Apply chrome changes')
    wait(lambda:'Use each item ID once' in text(),'duplicate chrome order rejected')
    assert settings.read_bytes() == saved_chrome_bytes
    button('Discard chrome changes')
    button('Reset chrome controls')
    button('Reset chrome draft')
    assert settings.read_bytes() == saved_chrome_bytes
    assert js("return document.getElementById('chrome-103').checked && document.getElementById('chrome-order-topbar').value===''")
    button('Discard chrome changes')
    input_value('chrome-order-sidebar', '1001, 1007')
    saved_chrome['chrome']['future_external'] = True
    saved_chrome['chrome']['topbar']['hidden'].append(109)
    settings.write_text(json.dumps(saved_chrome))
    external_chrome = settings.read_bytes()
    button('Apply chrome changes')
    wait(lambda:'Chrome preferences changed in another client' in text(),'stale chrome save rejected')
    assert settings.read_bytes() == external_chrome
    button('Reload chrome')
    wait(lambda:js("return !document.getElementById('chrome-109').checked"),'chrome external changes reloaded')
    assert not (ROOT/'application-launched').exists()
    (ROOT/'chrome.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    # Workspace edits preserve compositor-owned ratios and entries beyond the UI.
    workspace_document = json.loads(settings.read_text())
    workspace_document['workspaces'] = {'future':True,'split_ratios':{'DP-fixture':{'side_by_side':0.4,'stacked':0.6,'future':'keep'}},'workspace_names':{'12':'Unlisted workspace'},'workspace_wallpapers':{'12':'missing-legacy.exr'}}
    settings.write_text(json.dumps(workspace_document))
    before_workspaces = settings.read_bytes()
    wallpaper = ROOT/'workspace-wallpaper.png'
    wallpaper.write_bytes(base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aL1sAAAAASUVORK5CYII='))
    js("document.querySelector('nav button[aria-label=Workspaces]').click()")
    wait(lambda:js("return !!document.getElementById('workspace-slots')"),'native workspace page')
    input_value('workspace-slots','5')
    button('Discard workspace changes')
    assert settings.read_bytes() == before_workspaces
    input_value('workspace-slots','5')
    js("const e=document.getElementById('workspace-transition');e.value='materialize';e.dispatchEvent(new Event('change',{bubbles:true}));document.getElementById('workspace-split_screen_enabled').click();document.getElementById('workspace-restore_split_layouts').click()")
    def change_value(identifier,value):
        js('const e=document.getElementById('+json.dumps(identifier)+');e.value='+json.dumps(value)+';e.dispatchEvent(new Event("change",{bubbles:true}));')
    change_value('workspace-name-1','  Focus  ')
    change_value('workspace-wallpaper-1',str(wallpaper))
    button('Apply workspace changes')
    wait(lambda:'Session and split-layout restoration take effect at the next sign-in' in text(),'workspace save acknowledgement')
    workspace_saved = json.loads(settings.read_text())
    assert workspace_saved['workspaces']['max_workspace_slots'] == 5
    assert workspace_saved['workspaces']['transition_style'] == 'materialize'
    assert workspace_saved['workspaces']['split_screen_enabled'] is True
    assert workspace_saved['workspaces']['restore_split_layouts'] is True
    assert workspace_saved['workspaces']['workspace_names'] == {'1':'Focus','12':'Unlisted workspace'}
    assert workspace_saved['workspaces']['workspace_wallpapers'] == {'1':str(wallpaper),'12':'missing-legacy.exr'}
    assert workspace_saved['workspaces']['split_ratios'] == workspace_document['workspaces']['split_ratios']
    assert workspace_saved['workspaces']['future'] is True
    assert {k:v for k,v in workspace_saved.items() if k!='workspaces'} == {k:v for k,v in workspace_document.items() if k!='workspaces'}
    workspace_bytes = settings.read_bytes()
    change_value('workspace-wallpaper-2','https://example.com/wallpaper.png')
    button('Apply workspace changes')
    wait(lambda:'Choose an existing local' in text(),'nonlocal workspace wallpaper rejected')
    assert settings.read_bytes() == workspace_bytes
    button('Discard workspace changes')
    button('Reset workspace settings')
    button('Reset workspace draft')
    assert settings.read_bytes() == workspace_bytes
    assert js("return document.getElementById('workspace-name-1').value==='' && document.getElementById('workspace-wallpaper-1').value===''")
    button('Discard workspace changes')
    # Split divider writes are independent and must not cause stale edits or loss.
    change_value('workspace-name-2','Research')
    workspace_saved['workspaces']['split_ratios']['DP-fixture']['side_by_side'] = 0.7
    settings.write_text(json.dumps(workspace_saved))
    button('Apply workspace changes')
    wait(lambda:json.loads(settings.read_text())['workspaces']['workspace_names'].get('2')=='Research','workspace save alongside divider update')
    workspace_saved = json.loads(settings.read_text())
    assert workspace_saved['workspaces']['split_ratios']['DP-fixture']['side_by_side'] == 0.7
    wait(lambda:js("return Array.from(document.querySelectorAll('button')).find(e=>e.textContent==='Apply workspace changes').disabled"),'workspace draft saved')
    change_value('workspace-name-1','Draft name')
    workspace_saved['workspaces']['workspace_names']['1'] = 'External name'
    settings.write_text(json.dumps(workspace_saved))
    external_workspaces = settings.read_bytes()
    button('Apply workspace changes')
    wait(lambda:'Workspace preferences changed in another client' in text(),'stale workspace edit rejected')
    assert settings.read_bytes() == external_workspaces
    button('Reload workspaces')
    wait(lambda:js("return document.getElementById('workspace-name-1').value==='External name'"),'workspace reload')
    assert not (ROOT/'application-launched').exists()
    (ROOT/'workspaces.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    js("document.querySelector('nav button[aria-label=About]').click()")
    wait(lambda: not Path(f'/proc/{scan_pid}').exists(), 'page exit discovery cleanup')
    # Theme authoring and real Rust file/package operations stay isolated.
    before_editor=settings.read_bytes()
    js("document.querySelector('nav button[aria-label=\"Theme Editor\"]').click()")
    wait(lambda: js("return !!document.getElementById('theme-name')"),'native theme editor')
    js("const e=document.getElementById('theme-name');e.value='Native Editor Fixture';e.dispatchEvent(new Event('input',{bubbles:true}));const m=document.getElementById('theme-paint-mode');m.value='linear_gradient';m.dispatchEvent(new Event('change',{bubbles:true}))")
    button('Add gradient stop')
    wait(lambda: js("return !!document.getElementById('stop-position-2')"),'gradient stop added')
    js("const e=document.getElementById('theme-paint-mode');e.value='radial_gradient';e.dispatchEvent(new Event('change',{bubbles:true}))")
    wait(lambda: js("return getComputedStyle(document.querySelector('.theme-illustration')).backgroundImage.startsWith('radial-gradient')"),'native WebKit radial illustration')
    js("const e=document.getElementById('theme-paint-mode');e.value='linear_gradient';e.dispatchEvent(new Event('change',{bubbles:true}))")
    wait(lambda: js("return !!document.getElementById('theme-angle')"),'linear paint restored')
    js("const e=document.querySelector('.color-editor select');e.value='display_p3';e.dispatchEvent(new Event('change',{bubbles:true}))")
    wait(lambda: js("return !document.querySelector('.color-editor').disabled"),'Rust gamut conversion')
    button('Surfaces')
    js("const e=document.getElementById('theme-state');e.value='hover';e.dispatchEvent(new Event('change',{bubbles:true}))")
    wait(lambda: js("return !!document.getElementById('theme-inherit')"),'surface inheritance control')
    assert js("return document.getElementById('theme-inherit').checked")
    js("document.getElementById('theme-inherit').click()")
    wait(lambda: js("return !!document.querySelector('input[aria-label=\"bar hover R\"]')"),'surface override color')
    js("const e=document.querySelector('input[aria-label=\"bar hover R\"]');e.value='0.25';e.dispatchEvent(new Event('input',{bubbles:true}))")
    button('Layout & type')
    js("const e=document.getElementById('layout-bar_height');e.value='52';e.dispatchEvent(new Event('input',{bubbles:true}))")
    button('Wallpaper')
    js("const e=document.getElementById('wallpaper-dim');e.value='0.25';e.dispatchEvent(new Event('input',{bubbles:true}));document.getElementById('wallpaper-accent').click()")
    button('TOML')
    wait(lambda: js("return Array.from(document.querySelectorAll('button')).some(e=>e.textContent.trim()==='Generate TOML from draft'&&!e.disabled)"),'theme validation')
    button('Generate TOML from draft')
    wait(lambda:'Native Editor Fixture' in js("return document.getElementById('theme-source').value"),'Rust TOML generation')
    source=js("return document.getElementById('theme-source').value")
    parsed=tomllib.loads(source)
    assert parsed['paint']['mode']=='linear_gradient' and len(parsed['paint']['stops'])==3
    assert parsed['paint']['stops'][0]['color']['space']=='display_p3'
    assert parsed['semantic']['surfaces']['bar']['hover']['r']==0.25
    assert parsed['semantic']['layout']['bar_height']==52
    assert parsed['wallpaper']['dim']==0.25
    editor=invoke('theme_editor_file',{'action':{'type':'parse','source':source}})
    theme_path=ROOT/'authored.toml'
    saved_theme=invoke('theme_editor_file',{'action':{'type':'save','path':str(theme_path),'expected_hash':None,'document':editor['document']}})
    assert tomllib.loads(theme_path.read_text())['name']=='Native Editor Fixture'
    theme_path.write_text('external editor change')
    failure=invoke('theme_editor_file',{'action':{'type':'save','path':str(theme_path),'expected_hash':saved_theme['source']['hash'],'document':editor['document']}},ok=False)
    assert 'destination changed' in failure
    assert theme_path.read_text()=='external editor change'
    package_path=ROOT/'authored.fdtheme'
    invoke('theme_editor_file',{'action':{'type':'export','path':str(package_path),'expected_hash':None,'document':editor['document']}})
    package=invoke('theme_editor_file',{'action':{'type':'open','path':str(package_path)}})
    installed=invoke('theme_editor_file',{'action':{'type':'install','path':str(package_path),'expected_hash':package['source']['hash'],'overwrite':False}})
    assert Path(installed['source']['path']).is_relative_to(ROOT/'data/focaldesk/themes')
    assert 'already installed' in invoke('theme_editor_file',{'action':{'type':'install','path':str(package_path),'expected_hash':package['source']['hash'],'overwrite':False}},ok=False)
    assert settings.read_bytes()==before_editor
    button('Preview on desktop')
    wait(lambda:theme_state['preview_active'],'owned desktop theme preview')
    wait(lambda:'Desktop preview started for 30 seconds' in text(),'theme preview acknowledgment')
    theme_reject=True
    button('Apply to this session')
    wait(lambda:'Fixture rejected theme change' in text(),'rejected runtime theme change')
    assert theme_state['applied_revision']==0
    theme_reject=False
    button('Apply to this session')
    wait(lambda:theme_state['applied_revision']==1,'session theme apply')
    wait(lambda:'Applied to this session' in text(),'session theme acknowledgment')
    assert settings.read_bytes()==before_editor
    button('Preview on desktop')
    wait(lambda:theme_state['preview_active'],'theme preview before page exit')
    (ROOT/'theme-editor.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    js("document.querySelector('nav button[aria-label=About]').click()")
    wait(lambda:not theme_state['preview_active'],'owned theme preview cleanup on page exit')
    # Calendar uses an isolated connector host and a fake browser launcher.
    before_calendar=settings.read_bytes()
    js("document.querySelector('nav button[aria-label=Calendar]').click()")
    wait(lambda:'Design review <team>' in text(),'cached calendar events')
    assert 'Next 7 days' in text()
    aria_button('Manage calendars for work')
    wait(lambda:js("return !!document.getElementById('calendar-reminder')"),'calendar discovery')
    js("const e=document.getElementById('calendar-reminder');e.value=Array.from(e.options).find(o=>o.textContent==='15 minutes').value;e.dispatchEvent(new Event('change',{bubbles:true}))")
    button('Discard calendar changes')
    assert calendar_fixture.accounts[0]['reminder_minutes']==10
    js("const e=document.getElementById('calendar-reminder');e.value=Array.from(e.options).find(o=>o.textContent==='15 minutes').value;e.dispatchEvent(new Event('change',{bubbles:true}))")
    calendar_fixture.race_preferences=True
    button('Save calendar preferences')
    wait(lambda:'Calendar preferences changed in another client' in text(),'server calendar preference compare-and-set')
    assert calendar_fixture.accounts[0]['reminder_minutes']==30
    button('Reload calendars')
    wait(lambda:js("return document.getElementById('calendar-reminder').selectedOptions[0].textContent==='30 minutes'"),'reload external preference edit')
    wait(lambda:js("return !document.querySelector('fieldset').disabled"),'calendar reload ready')
    js("document.getElementById('calendar-choice-1').click()")
    button('Save calendar preferences')
    wait(lambda:'Calendars removed from sync' in text(),'calendar cache-removal confirmation')
    button('Keep current settings')
    assert calendar_fixture.accounts[0]['calendar_ids']==['primary','work']
    button('Save calendar preferences')
    button('Confirm calendar change')
    wait(lambda:calendar_fixture.accounts[0]['calendar_ids']==['primary'],'calendar selection save')
    wait(lambda:'Calendar selection and reminders saved' in text(),'calendar save acknowledgement')
    aria_button('Sync work')
    wait(lambda:calendar_fixture.syncs==1,'calendar sync queued')
    wait(lambda:'Sync queued' in text(),'queued sync status')
    wait(lambda:js("return !document.querySelector('button[aria-label=\"Disconnect calendar work\"]').disabled"),'calendar account ready')
    aria_button('Disconnect calendar work')
    button('Confirm calendar change')
    wait(lambda:not calendar_fixture.accounts[0]['connected'],'calendar disconnect with cache retained')
    assert len(calendar_fixture.events)==1
    wait(lambda:'cached events retained' in text(),'retained-cache disconnect status')
    js("const e=document.getElementById('calendar-account');e.value='new-google';e.dispatchEvent(new Event('input',{bubbles:true}));const c=document.getElementById('calendar-client');c.value='fixture-client';c.dispatchEvent(new Event('input',{bubbles:true}));const s=document.getElementById('calendar-secret');s.value='fixture-app-secret';s.dispatchEvent(new Event('input',{bubbles:true}))")
    button('Start browser sign-in')
    wait(lambda:(calendar_fixture.root/'browser.jsonl').exists(),'Rust browser launch')
    wait(lambda:'Browser sign-in' in text(),'calendar browser sign-in status')
    assert js("return document.getElementById('calendar-secret').value") == ''
    assert 'fixture-app-secret' not in text() and 'fixture-state' not in text()
    assert 'accounts.google.com' not in text()
    first_flow=list(calendar_fixture.flows)[-1]
    button('Stop waiting')
    time.sleep(.3)
    polls=calendar_fixture.flows[first_flow]['polls']
    time.sleep(2.2)
    assert calendar_fixture.flows[first_flow]['polls']==polls
    calendar_fixture.complete(first_flow)
    button('Resume waiting')
    wait(lambda:'Account connected' in text(),'Google OAuth completion')
    wait(lambda:js("return !!document.querySelector('button[aria-label=\"Manage calendars for new-google\"]')"),'connected account refresh')
    assert not any('fixture-app-secret' in json.dumps(call) for call in calendar_fixture.calls)
    calendar_fixture.bad_url=True
    js("const e=document.getElementById('calendar-account');e.value='bad-url';e.dispatchEvent(new Event('input',{bubbles:true}))")
    browser_before=(calendar_fixture.root/'browser.jsonl').read_text()
    button('Start browser sign-in')
    wait(lambda:'unsupported sign-in link' in text(),'calendar sign-in URL validation')
    assert (calendar_fixture.root/'browser.jsonl').read_text()==browser_before
    calendar_fixture.bad_url=False
    js("const p=document.getElementById('calendar-provider');p.value='microsoft';p.dispatchEvent(new Event('change',{bubbles:true}));const e=document.getElementById('calendar-account');e.value='work-m365';e.dispatchEvent(new Event('input',{bubbles:true}))")
    wait(lambda:js("return !!document.getElementById('calendar-tenant')"),'Microsoft tenant field')
    button('Start browser sign-in')
    wait(lambda:len(calendar_fixture.flows)==3,'Microsoft OAuth started')
    microsoft_flow=list(calendar_fixture.flows)[-1]
    calendar_fixture.complete(microsoft_flow,success=False)
    wait(lambda:'Sign-in failed or expired' in text(),'Microsoft OAuth error')
    assert 'must-not-reach-the-webview' not in text()
    (ROOT/'calendar.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    # Deleting cached data requires the explicit option plus confirmation.
    wait(lambda:js("return !document.querySelector('button[aria-label=\"Disconnect calendar work\"]').disabled"),'calendar ready for cache deletion')
    js("const row=Array.from(document.querySelectorAll('.account-row')).find(row=>row.querySelector('strong').textContent==='work');row.querySelector('input[type=checkbox]').click()")
    aria_button('Disconnect calendar work')
    button('Confirm calendar change')
    wait(lambda:not calendar_fixture.events,'explicit calendar cache purge')
    wait(lambda:'local calendar data removed' in text(),'cache deletion acknowledgement')
    assert settings.read_bytes()==before_calendar
    calendar_fixture.offline=True
    button('Refresh calendar')
    wait(lambda:'Fixture calendar service unavailable' in text(),'calendar service error state')
    calendar_fixture.offline=False
    button('Refresh calendar')
    wait(lambda:'No cached events for the next 7 days' in text(),'calendar empty-cache state')
    # Printer commands use private CUPS executables; no pages are submitted.
    before_printers=settings.read_bytes()
    js("document.querySelector('nav button[aria-label=Printers]').click()")
    wait(lambda:'Office-Printer' in text() and 'Photo_Printer' in text(),'CUPS configured queues')
    printer_state=lambda:json.loads((printer_root/'state.json').read_text())
    printer_calls=lambda:[json.loads(line) for line in (printer_root/'calls.jsonl').read_text().splitlines()]
    def change_printer_state(change):
        state=printer_state()
        change(state)
        temporary=printer_root/'state-from-test.tmp'
        temporary.write_text(json.dumps(state))
        temporary.replace(printer_root/'state.json')
    assert 'fixture-uri-secret' not in text() and 'password=hidden' not in text()
    assert not any(call['tool']=='lpinfo' for call in printer_calls())
    aria_button('Make Photo_Printer default')
    button('Keep current state')
    assert printer_state()['default']=='Office-Printer'
    aria_button('Make Photo_Printer default')
    button('Confirm printer action')
    wait(lambda:printer_state()['default']=='Photo_Printer','CUPS default update')
    wait(lambda:'Default printer updated' in text(),'printer default acknowledgement')
    (printer_root/'reject').touch()
    aria_button('Pause Office-Printer')
    button('Confirm printer action')
    wait(lambda:'Fixture CUPS authorization denied' in text(),'CUPS authorization rejection')
    assert printer_state()['printers'][0]['enabled'] is True
    (printer_root/'reject').unlink()
    aria_button('Pause Office-Printer')
    button('Confirm printer action')
    wait(lambda:not printer_state()['printers'][0]['enabled'],'printer pause')
    aria_button('Resume Office-Printer')
    assert printer_state()['printers'][0]['enabled'] is False
    button('Confirm printer action')
    wait(lambda:printer_state()['printers'][0]['enabled'],'printer resume')
    aria_button('Reject jobs for Office-Printer')
    button('Confirm printer action')
    wait(lambda:not printer_state()['printers'][0]['accepting'],'rejecting new print jobs')
    aria_button('Accept jobs for Office-Printer')
    wait(lambda:printer_state()['printers'][0]['accepting'],'accepting new print jobs')
    aria_button('View jobs for Office-Printer')
    wait(lambda:'Office-Printer-42' in text(),'CUPS job list')
    aria_button('Cancel print job Office-Printer-42')
    button('Keep current state')
    assert printer_state()['cancelled']==[]
    aria_button('Cancel print job Office-Printer-42')
    button('Confirm printer action')
    wait(lambda:printer_state()['cancelled']==['Office-Printer-42'],'individual print job cancelled')
    wait(lambda:'Office-Printer-42' not in text(),'cancelled job removed from view')
    aria_button('Cancel print job Office-Printer-43')
    (printer_root/'vanish-job').touch()
    button('Confirm printer action')
    wait(lambda:'already finished or disappeared' in text(),'stale print job rejection')
    assert printer_state()['cancelled']==['Office-Printer-42']
    wait(lambda:'No unfinished print jobs' in text(),'empty job queue state')
    button('Close jobs')
    button('Find printers')
    wait(lambda:'Printer discovery complete' in text(),'explicit CUPS discovery')
    discovered=invoke('get_printers',{})['discovered']
    assert len(discovered)==1
    aria_button('Configure discovered printer '+discovered[0]['suggested_name'])
    wait(lambda:js("return document.activeElement.id==='printer-name'"),'printer installation keyboard focus')
    js("const e=document.getElementById('printer-name');e.value='Studio_Printer';e.dispatchEvent(new Event('input',{bubbles:true}))")
    button('Install printer')
    wait(lambda:any(printer['name']=='Studio_Printer' for printer in printer_state()['printers']),'discovered printer installation')
    wait(lambda:'Printer installation requested' in text(),'printer install acknowledgement')
    lpadmin_before=len([call for call in printer_calls() if call['tool']=='lpadmin'])
    for name,uri in [('Studio_Printer','ipp://printer.local/ipp/print'),('Unsafe','file:///tmp/output'),('--help','ipp://printer.local/ipp/print'),('Unsafe','ipp://user:private@printer.local/ipp/print')]:
        invoke('apply_printer',{'action':{'type':'install','name':name,'uri':uri,'model':'everywhere'}},ok=False)
    assert len([call for call in printer_calls() if call['tool']=='lpadmin'])==lpadmin_before
    button('Load installed drivers')
    wait(lambda:js("return Array.from(document.getElementById('printer-driver').options).some(option=>option.value==='drv:///fixture.drv/model.ppd')"),'registered CUPS driver list')
    js("for(const [id,value] of [['printer-name','Legacy_Printer'],['printer-uri','socket://legacy.local:9100']]){const e=document.getElementById(id);e.value=value;e.dispatchEvent(new Event('input',{bubbles:true}));}const driver=document.getElementById('printer-driver');driver.value='drv:///fixture.drv/model.ppd';driver.dispatchEvent(new Event('change',{bubbles:true}))")
    wait(lambda:js("return !Array.from(document.querySelectorAll('button')).find(e=>e.textContent.trim()==='Install printer').disabled"),'printer model selection ready')
    button('Install printer')
    wait(lambda:any(printer['name']=='Legacy_Printer' for printer in printer_state()['printers']),'manual printer installation')
    wait(lambda:'Legacy_Printer' in text(),'manual printer list refresh')
    assert next(printer for printer in printer_state()['printers'] if printer['name']=='Legacy_Printer')['model']=='drv:///fixture.drv/model.ppd'
    invoke('apply_printer',{'action':{'type':'install','name':'Vendor_Printer','uri':'vendorprint:/usb/Fixture','model':'drv:///fixture.drv/model.ppd'}})
    assert any(printer['name']=='Vendor_Printer' for printer in printer_state()['printers'])
    invoke('apply_printer',{'action':{'type':'install','name':'Unknown_Backend','uri':'notinstalled:/usb/Fixture','model':'drv:///fixture.drv/model.ppd'}},ok=False)
    assert not any(printer['name']=='Unknown_Backend' for printer in printer_state()['printers'])
    invoke('apply_printer',{'action':{'type':'install','name':'Raw_Printer','uri':'socket://raw.local:9100','model':'raw'}})
    assert next(printer for printer in printer_state()['printers'] if printer['name']=='Raw_Printer')['model']=='raw'
    (ROOT/'printers.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    (printer_root/'accepting-error').touch()
    button('Refresh printers')
    wait(lambda:'Could not read whether queues accept new jobs' in text(),'unknown printer acceptance warning')
    assert js("return Array.from(document.querySelectorAll('button')).filter(e=>e.textContent==='Acceptance unknown').every(e=>e.disabled)")
    (printer_root/'accepting-error').unlink()
    (printer_root/'lpstat-hang').touch()
    button('Refresh printers')
    wait(lambda:'lpstat timed out' in text(),'bounded CUPS helper timeout')
    (printer_root/'lpstat-hang').unlink()
    change_printer_state(lambda state:state.update(running=False))
    button('Refresh printers')
    wait(lambda:'CUPS print service is not running' in text(),'stopped print service state')
    change_printer_state(lambda state:state.update(running=True,printers=[],default=None,jobs=[]))
    button('Refresh printers')
    wait(lambda:'No printers are configured' in text(),'empty CUPS destination state')
    assert settings.read_bytes()==before_printers
    assert all(call['tool']!='lp' for call in printer_calls())
    # Debug opens only a Rust-located log; the report never includes log contents.
    debug_document = json.loads(settings.read_text())
    debug_document['debug'] = {'future':'preserved'}
    settings.write_text(json.dumps(debug_document))
    before_debug = settings.read_bytes()
    js("document.querySelector('nav button[aria-label=Debug]').click()")
    wait(lambda:js("return !!document.getElementById('debug-level')"),'native debug page')
    report = js("return document.getElementById('debug-report').value")
    assert 'FocalDesk diagnostics' in report and 'Version:' in report and 'OS/arch:' in report
    assert str(settings) in report and str(debug_log) in report
    assert 'fixture-private-log-contents-must-not-enter-webview' not in report
    assert 'fixture-private-log-contents-must-not-enter-webview' not in text()
    js("document.getElementById('debug-show_fps').click()")
    button('Discard debug changes')
    assert settings.read_bytes() == before_debug
    assert not js("return document.getElementById('debug-show_fps').checked")
    js("const e=document.getElementById('debug-level');e.value='debug';e.dispatchEvent(new Event('change',{bubbles:true}));for(const key of ['show_fps','show_damage_regions','show_input_events','verbose_protocol_logs'])document.getElementById('debug-'+key).click()")
    button('Apply debug changes')
    wait(lambda:'Preferences saved' in text(),'debug save acknowledgement')
    debug_saved = json.loads(settings.read_text())
    assert debug_saved['debug'] == dict(future='preserved',log_level='debug',show_fps=True,show_damage_regions=True,show_input_events=True,verbose_protocol_logs=True)
    assert {k:v for k,v in debug_saved.items() if k!='debug'} == {k:v for k,v in debug_document.items() if k!='debug'}
    debug_bytes = settings.read_bytes()
    button('Reset debug settings')
    button('Reset debug draft')
    assert settings.read_bytes() == debug_bytes
    assert js("return document.getElementById('debug-level').value==='info' && !document.getElementById('debug-show_fps').checked")
    button('Discard debug changes')
    js("document.getElementById('debug-show_fps').click()")
    debug_saved['debug']['log_level'] = 'warn'
    settings.write_text(json.dumps(debug_saved))
    external_debug = settings.read_bytes()
    button('Apply debug changes')
    wait(lambda:'Debug preferences changed in another client' in text(),'stale debug save rejected')
    assert settings.read_bytes() == external_debug
    button('Reload diagnostics')
    wait(lambda:js("return document.getElementById('debug-level').value==='warn'"),'debug reload')
    current_debug = invoke('get_debug',{})['preferences']
    invoke('apply_debug',{'change':{'expected':current_debug,'requested':dict(current_debug,log_level='invalid')}},ok=False)
    assert settings.read_bytes() == external_debug
    assert not (ROOT/'application-launched').exists()
    js("document.querySelector('nav button[aria-label=Sound]').click()")
    wait(lambda: 'Test speakers' in text(), 'speaker control')
    (audio_root/'speaker-hang').touch()
    button('Test speakers')
    try: wait(lambda:(audio_root/'speaker.jsonl').exists(),'speaker playback fixture')
    except AssertionError: raise AssertionError(text())
    speaker_pid=json.loads((audio_root/'speaker.jsonl').read_text().splitlines()[-1])['pid']
    button('Stop speaker test')
    wait(lambda:not Path(f'/proc/{speaker_pid}').exists(),'speaker stop cleanup')
    button('Test speakers')
    wait(lambda:len((audio_root/'speaker.jsonl').read_text().splitlines())==2,'second speaker test')
    speaker_pid=json.loads((audio_root/'speaker.jsonl').read_text().splitlines()[-1])['pid']
    js("document.querySelector('nav button[aria-label=About]').click()")
    wait(lambda:not Path(f'/proc/{speaker_pid}').exists(),'speaker page exit cleanup')
    js("document.querySelector('nav button[aria-label=Sound]').click()")
    wait(lambda:'Test speakers' in text(),'speaker timeout page')
    button('Test speakers')
    wait(lambda:len((audio_root/'speaker.jsonl').read_text().splitlines())==3,'bounded speaker test')
    speaker_pid=json.loads((audio_root/'speaker.jsonl').read_text().splitlines()[-1])['pid']
    wait(lambda:not Path(f'/proc/{speaker_pid}').exists(),'speaker backend deadline without client stop')
    wait(lambda:'Control operation timed out' in text(),'speaker timeout surfaced')
    js("document.querySelector('nav button[aria-label=About]').click()")

    assert 'ALPHA' in text()
    js("document.querySelector('nav button[aria-label=Debug]').click()")
    wait(lambda:'Open log file' in text(),'return to debug page')
    button('Open log file')
    wait(lambda:'Requested opening the log file' in text(),'native log-open request')
    wait(lambda:(ROOT/'application-launched').exists(),'private registered application opened explicitly')
    assert settings.read_bytes() == external_debug
    (ROOT/'debug.png').write_bytes(base64.b64decode(wd('GET','/screenshot')))
    js("document.querySelector('nav button[aria-label=About]').click()")
    wait(lambda: "Operating system" in text(), "About system data")

    (ROOT / "about.png").write_bytes(base64.b64decode(wd("GET", "/screenshot")))
    print("PASS: real WebKit appearance, Displays, Sound, microphone cleanup, Keyboard/Input, complete sidebar, Privacy/Power, Accessibility keyboard navigation, Network/Wi-Fi secrets, Bluetooth passkey/PIN/device controls and cleanup, atomic saves, preservation, reset, stale edit rejection, Theme Editor authoring/files/packages/runtime/cleanup, Calendar accounts/preferences/OAuth/disconnect/cache/error states, Printers discovery/install/drivers/queue controls/jobs/validation/errors, Applications defaults/rules/startup/row metadata/stale edits/private GIO associations/no execution, Chrome visibility/order/preservation/reset/stale edits, Workspaces behavior/names/wallpapers/reset/split-ratio preservation/stale edits, Debug logging/report/native log opening/reset/preservation/stale edits, and About")
finally:
    if session:
        try:
            (ROOT / 'last-page.txt').write_text(text())
            (ROOT / 'last-page.png').write_bytes(base64.b64decode(wd('GET', '/screenshot')))
        except Exception:
            pass
        try:
            request("DELETE", f"/session/{session}")
        except Exception:
            pass
    for child in children:
        child.terminate()
        child.wait(timeout=10)
    print(f"Artifacts: {ROOT}")
