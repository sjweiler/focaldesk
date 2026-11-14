"""Exercise real compositor display trials after the requesting CLI exits.

Uses a private nested FocalDesk instance and private configuration; physical
monitor modes and the host desktop are never changed. Requires a desktop session
or a headless Wayland host supplied through the normal environment.
"""
import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import tomllib
from icc_fixture import write_srgb

REPO = Path(__file__).resolve().parents[3]
artifact_base = os.environ.get("FOCALDESK_DISPLAY_TEST_ARTIFACTS")
if artifact_base:
    Path(artifact_base).mkdir(parents=True, exist_ok=True)
ROOT = Path(tempfile.mkdtemp(prefix="focaldesk-display-runtime-", dir=artifact_base)).resolve()
children = []
env = os.environ.copy()
if not env.get("WAYLAND_DISPLAY") and not env.get("DISPLAY"):
    result = subprocess.run(["systemctl", "--user", "show-environment"], capture_output=True, text=True, check=True)
    for line in result.stdout.splitlines():
        key, _, value = line.partition("=")
        if key in {"WAYLAND_DISPLAY", "DISPLAY", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "XDG_SESSION_TYPE"}:
            env[key] = value
for kind in ["config", "data", "cache", "state"]:
    (ROOT / kind).mkdir()
(ROOT / "config/focaldesk").mkdir()
settings = ROOT / "config/focaldesk/settings.json"
settings.write_text(json.dumps({"desktop_config": {}, "display_test_unrelated": "preserved"}))
env.update({"XDG_CONFIG_HOME": str(ROOT / "config"), "XDG_DATA_HOME": str(ROOT / "data"),
            "XDG_CACHE_HOME": str(ROOT / "cache"), "XDG_STATE_HOME": str(ROOT / "state"),
            "FOCALDESK_DESKTOP_SOCKET_PATH": str(ROOT / "desktop.sock"), "FOCALDESK_DISABLE_PORTAL_ENV": "1",
            "FOCALDESK_LOG_FILE": str(ROOT / "compositor.log"), "LIBGL_ALWAYS_SOFTWARE": "1"})


def cli(*args, ok=True):
    result = subprocess.run([str(REPO / "target/debug/focaldesk-cli"), *map(str, args)], env=env, capture_output=True, text=True, timeout=15)
    if ok:
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)
    assert result.returncode != 0, result.stdout
    return result.stderr


def wait(predicate, description, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.2)
    raise AssertionError(description)


def preview(baseline, scale):
    expected = [output["config"] for output in baseline["outputs"]]
    requested = copy.deepcopy(expected)
    requested[0]["scale"] = scale
    path = ROOT / "trial.json"
    path.write_text(json.dumps({"expected": expected, "outputs": requested}))
    return cli("preview-displays", path)


try:
    log = open(ROOT / "compositor.stderr", "wb")
    compositor = subprocess.Popen([str(REPO / "target/debug/focaldesk-desktop")], env=env, stdout=log, stderr=subprocess.STDOUT)
    children.append(compositor)
    wait(lambda: (ROOT / "desktop.sock").exists() and compositor.poll() is None, "nested compositor did not start")
    baseline = wait(lambda: (state if (state := cli("display-configuration"))["outputs"] else None), "nested output is unavailable")
    # Host tiling/configure events can arrive after the first mapped frame.
    previous=None
    stable_since=time.monotonic()
    def stable_output():
        global previous,stable_since
        current=cli('display-configuration')
        if current['outputs']!=previous:
            previous=copy.deepcopy(current['outputs'])
            stable_since=time.monotonic()
        return current if time.monotonic()-stable_since>=2 else None
    baseline=wait(stable_output,'nested output configuration did not settle')
    original = settings.read_bytes()
    # Valid ICC changes travel through the actual compositor trial, never a fixture.
    profile=ROOT/'private-srgb.icc'
    write_srgb(profile)
    expected=[output['config'] for output in baseline['outputs']]
    requested=copy.deepcopy(expected)
    requested[0]['icc_profile_path']=str(profile)
    path=ROOT/'icc-trial.json'
    path.write_text(json.dumps({'expected':expected,'outputs':requested}))
    result=cli('preview-displays',path)
    assert settings.read_bytes()==original
    # The current trial cannot display a pattern on an SDR output; stale IDs fail.
    cli('display-trial-calibration-pattern',result['preview']['id']+1,expected[0]['connector'],'overview',ok=False)
    cli('display-trial-calibration-pattern',result['preview']['id'],expected[0]['connector'],'overview',ok=False)
    write_srgb(profile,2.1)
    assert 'ICC profile changed' in cli('confirm-displays',result['preview']['id'],ok=False)
    wait(lambda:not cli('display-configuration')['preview'],'ICC file-change rollback')
    assert settings.read_bytes()==original
    # A profile that stays unchanged can be kept and subsequently cleared.
    expected=[output['config'] for output in cli('display-configuration')['outputs']]
    requested=copy.deepcopy(expected);requested[0]['icc_profile_path']=str(profile)
    path.write_text(json.dumps({'expected':expected,'outputs':requested}))
    result=cli('preview-displays',path)
    cli('confirm-displays',result['preview']['id'])
    assert json.loads(settings.read_text())['displays']['outputs'][0]['icc_profile_path']==str(profile)
    expected=[output['config'] for output in cli('display-configuration')['outputs']]
    requested=copy.deepcopy(expected);requested[0]['icc_profile_path']=None
    path.write_text(json.dumps({'expected':expected,'outputs':requested}))
    result=cli('preview-displays',path);cli('confirm-displays',result['preview']['id'])
    original=settings.read_bytes()
    baseline=cli('display-configuration')
    # A nested session cannot disable the host output or reserve its only desktop.
    expected=[output['config'] for output in baseline['outputs']]
    for key,value in [('enabled',False),('role','ControlPanel')]:
        requested=copy.deepcopy(expected)
        requested[0][key]=value
        path=ROOT/'unsafe-trial.json'
        path.write_text(json.dumps({'expected':expected,'outputs':requested}))
        cli('preview-displays',path,ok=False)
        assert cli('display-configuration')['preview'] is None
        assert settings.read_bytes()==original
    initial_scale = baseline["outputs"][0]["config"]["scale"]
    test_scale = 1.25 if initial_scale != 1.25 else 1.5
    result = preview(baseline, test_scale)
    trial_id = result["preview"]["id"]
    assert result["preview"]["phase"] == "confirming", result
    assert settings.read_bytes() == original
    # The CLI process is already gone. No frontend process or timer remains.
    restored = wait(lambda: (state if not (state := cli("display-configuration"))["preview"] else None), "compositor did not expire the abandoned trial", timeout=26)
    assert restored["outputs"][0]["config"]["scale"] == initial_scale
    assert settings.read_bytes() == original
    cli("confirm-displays", trial_id, ok=False)
    result = preview(restored, test_scale)
    cli("confirm-displays", result["preview"]["id"] + 1, ok=False)
    confirmed = cli("confirm-displays", result["preview"]["id"])
    assert confirmed["preview"] is None
    saved = json.loads(settings.read_text())
    assert saved["displays"]["outputs"][0]["scale"] == test_scale
    assert saved["display_test_unrelated"] == "preserved"
    confirmed_bytes = settings.read_bytes()
    result = preview(confirmed, initial_scale)
    reverted = cli("revert-displays", result["preview"]["id"])
    wait(lambda: not cli("display-configuration")["preview"], "explicit rollback did not finish")
    assert settings.read_bytes() == confirmed_bytes
    assert cli("display-configuration")["outputs"][0]["config"]["scale"] == test_scale
    # A later unrelated preferences update must invalidate confirmation.
    result = preview(cli("display-configuration"), initial_scale)
    changed = json.loads(settings.read_text())
    changed["displays"]["outputs"][0]["x"] += 1
    settings.write_text(json.dumps(changed))
    assert "changed in another client" in cli("confirm-displays", result["preview"]["id"], ok=False)
    wait(lambda: not cli("display-configuration")["preview"], "conflict rollback did not finish")
    # Theme preview recovery is also owned by the actual compositor, not the UI.
    def theme(command, ok=True):
        path=ROOT/'theme-command.json'
        path.write_text(json.dumps(command))
        return cli('theme-editor-command',path,ok=ok)
    document=tomllib.loads((REPO/'assets/themes/default.toml').read_text())
    document['name']='Isolated theme trial'
    document['wallpaper']['path']=None
    before_theme=settings.read_bytes()
    baseline=cli('theme-editor-status')
    token=12345
    assert theme({'command':'preview_owned','token':token,'document':document})['preview_active']
    assert 'another theme editor' in theme({'command':'revert_owned','token':token+1},ok=False)
    assert 'another theme editor' in theme({'command':'preview_owned','token':token+1,'document':document},ok=False).lower()
    wait(lambda:not cli('theme-editor-status')['preview_active'],'theme preview did not expire after the requesting process exited',timeout=36)
    assert settings.read_bytes()==before_theme
    revision=baseline['applied_revision']
    applied=theme({'command':'apply_owned','token':token,'expected_revision':revision,'document':document})
    assert applied['applied_revision']==revision+1
    assert 'changed in another editor' in theme({'command':'apply_owned','token':token,'expected_revision':revision,'document':document},ok=False)
    assert settings.read_bytes()==before_theme
    print("PASS: real compositor display/theme deadlines after client exit, ownership, stale revisions, confirmation, rollback, and persistence isolation")
finally:
    for child in children:
        child.terminate()
        try:
            child.wait(timeout=10)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
    print(f"Artifacts: {ROOT}")
