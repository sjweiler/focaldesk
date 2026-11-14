"""Test capture deadline, stalled streams, and parent crash using synthetic PCM."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
from audio_fixture import install

REPO = Path(__file__).resolve().parents[3]
ROOT = Path(tempfile.mkdtemp(prefix='focaldesk-audio-runtime-'))
fixture_bin = install(ROOT)
env = {**os.environ, 'PATH': str(fixture_bin) + os.pathsep + os.environ['PATH'], 'FOCALDESK_AUDIO_FIXTURE': str(ROOT)}
probe = str(REPO / 'target/debug/examples/audio_probe')
children = []


def running(pid):
    try:
        # A briefly unreaped child is already terminated, not a live capture.
        return Path(f'/proc/{pid}/stat').read_text().split(') ', 1)[1][0] != 'Z'
    except FileNotFoundError:
        return False


def wait(predicate, label, seconds=4):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate(): return
        time.sleep(.05)
    raise AssertionError(label)


def capture_pid():
    return json.loads((ROOT / 'capture.jsonl').read_text().splitlines()[-1])['pid']


try:
    snapshot = subprocess.run([probe, 'snapshot'], env=env, capture_output=True, text=True, check=True)
    assert len(json.loads(snapshot.stdout)['inputs']) == 2
    assert not (ROOT / 'capture.jsonl').exists(), 'inventory must not start microphone capture'
    # The Rust deadline is authoritative even without UI polling or Stop.
    started = time.monotonic()
    bounded = subprocess.Popen([probe, 'meter', 'input:3:usb.mic'], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    children.append(bounded)
    initial = json.loads(bounded.stdout.readline())
    assert initial['active'] and initial['remaining_ms'] == 20000
    wait(lambda: (ROOT / 'capture.jsonl').exists(), 'synthetic capture did not start')
    pid = capture_pid()
    remainder, errors = bounded.communicate(timeout=24)
    assert bounded.returncode == 0, errors
    final = json.loads(remainder.strip())
    assert not final['active'] and final['remaining_ms'] == 0 and final['error'] is None
    assert 19 <= time.monotonic() - started <= 24
    wait(lambda: not running(pid), 'capture helper survived its deadline')
    # A stalled helper must also die when the parent crashes, with no JS cleanup.
    (ROOT / 'capture-stall').touch()
    crashed = subprocess.Popen([probe, 'meter', 'input:3:usb.mic'], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    children.append(crashed)
    json.loads(crashed.stdout.readline())
    wait(lambda: capture_pid() != pid, 'second synthetic capture did not start')
    child_pid = capture_pid()
    crashed.kill()
    crashed.wait(timeout=5)
    wait(lambda: not running(child_pid), 'microphone capture survived a Settings-process crash')
    # Control commands are bounded when the server stops responding.
    (ROOT / 'hang').touch()
    started = time.monotonic()
    hung = subprocess.run([probe, 'snapshot'], env=env, capture_output=True, text=True, timeout=5)
    assert hung.returncode != 0 and 'two seconds' in hung.stderr
    assert time.monotonic() - started < 4
    print('PASS: read-only inventory, monitor filtering, 20-second capture deadline, parent-crash cleanup, stalled-stream cleanup, and audio-server timeout')
finally:
    for child in children:
        if child.poll() is None: child.kill(); child.wait()
    print(f'Artifacts: {ROOT}')
