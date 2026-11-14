<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { getAudio, applyAudio, startInputTest, getInputLevel, stopInputTest } from './api';
  import type { AudioSnapshot, AudioAction, InputLevel } from './api';
  import { meterPercent, volumeLabel } from './audio-level';

  let snapshot = $state<AudioSnapshot | null>(null);
  let loading = $state(true);
  let busy = $state(false);
  let error = $state('');
  let message = $state('');
  let outputVolume = $state(0);
  let inputVolume = $state(0);
  let editing = $state(false);
  let inputLevel = $state<InputLevel | null>(null);
  let testing = $state(false), speakerTesting = $state(false), speakerStarting = $state(false);
  let speakerEpoch = 0;
  let alive = true, polling = false, levelPolling = false, revision = 0;
  let output = $derived(snapshot?.outputs.find(device => device.is_default));
  let input = $derived(snapshot?.inputs.find(device => device.is_default));
  let level = $derived(meterPercent(inputLevel?.active ? inputLevel.rms : 0));

  async function speakerTest() {
    if (!output || speakerTesting) return;
    error = ''; speakerStarting = true; speakerTesting = true; const epoch = ++speakerEpoch;
    try { await invoke('start_speaker_test', { deviceId: output.id }); if (!alive || epoch !== speakerEpoch) await invoke('stop_speaker_test'); }
    catch (cause) { if (alive && epoch === speakerEpoch) { error = String(cause); speakerTesting = false; } }
    finally { speakerStarting = false; }
  }
  async function stopSpeaker() { ++speakerEpoch; speakerTesting = false; await invoke('stop_speaker_test').catch(() => {}); }
  async function pollSpeaker() {
    if (!speakerTesting || speakerStarting) return;
    try { speakerTesting = await invoke<boolean>('speaker_test_status'); }
    catch (cause) { error = String(cause); speakerTesting = false; }
  }
  function accept(next: AudioSnapshot) {
    snapshot = next;
    outputVolume = Math.min(150, next.outputs.find(device => device.is_default)?.volume_percent ?? 0);
    inputVolume = Math.min(100, next.inputs.find(device => device.is_default)?.volume_percent ?? 0);
    const nextInput = next.inputs.find(device => device.is_default);
    if (inputLevel?.active && (inputLevel.device_id !== nextInput?.id || nextInput?.muted)) void stopTest();
  }
  async function refresh(clear = false) {
    if (polling || busy || editing) return;
    const token = revision; polling = true;
    try {
      const next = await getAudio();
      if (alive && token === revision && !busy && !editing) { accept(next); if (clear) error = ''; }
    } catch (cause) { if (alive && token === revision) { error = String(cause); message = ''; } }
    finally { polling = false; if (alive) loading = false; }
  }
  async function update(action: AudioAction) {
    if (busy) return;
    await stopSpeaker();
    revision++; busy = true; editing = false; error = ''; message = '';
    try {
      const next = await applyAudio(action);
      if (alive) { accept(next); message = 'Sound settings updated.'; }
    } catch (cause) {
      if (alive) {
        error = String(cause);
        if (snapshot) accept(snapshot);
      }
    } finally { if (alive) busy = false; }
  }
  async function startTest() {
    if (!input || testing || busy) return;
    testing = true; error = ''; message = '';
    try {
      const next = await startInputTest(input.id);
      if (alive) inputLevel = next;
      else void stopInputTest(next.id).catch(() => {});
    } catch (cause) { if (alive) error = String(cause); }
    finally { if (alive) testing = false; }
  }
  async function stopTest() {
    const id = inputLevel?.id;
    inputLevel = null;
    if (id !== undefined) {
      try { await stopInputTest(id); }
      catch { /* Window blur or a completed cleanup may already have stopped it. */ }
    }
  }
  async function pollLevel() {
    if (!inputLevel?.active || levelPolling) return;
    const id = inputLevel.id; levelPolling = true;
    try {
      const next = await getInputLevel(id);
      if (alive && inputLevel?.id === id) {
        inputLevel = next;
        if (next.error) error = next.error;
        else if (!next.active) message = 'Microphone test finished.';
      }
    } catch { if (alive && inputLevel?.id === id) inputLevel = null; }
    finally { levelPolling = false; }
  }
  onMount(() => {
    void refresh();
    const devices = window.setInterval(() => void refresh(), 1500);
    const meter = window.setInterval(() => { void pollLevel(); void pollSpeaker(); }, 150);
    return () => {
      alive = false; window.clearInterval(devices); window.clearInterval(meter);
      void stopTest(); void stopSpeaker();
    };
  });
</script>

<svelte:window onblur={() => { editing = false; void stopTest(); void stopSpeaker(); }} onfocus={() => void refresh()} />

<div class="page-heading"><div><div class="eyebrow">MAKE YOURSELF HEARD</div><h1>Sound</h1><p>Choose how you listen, and how the world hears you.</p></div><button class="quiet" disabled={busy || loading} onclick={() => refresh(true)}>Refresh devices</button></div>
{#if error}<div class="notice error" role="alert">{error}</div>{/if}
{#if message}<div class="notice success" role="status">{message}</div>{/if}
{#if loading}<div class="loading" role="status">Connecting to your audio devices…</div>
{:else if snapshot}
  <section class="audio-hero" aria-label="Current audio route"><span class="audio-hero-icon" aria-hidden="true">♫</span><div><span class="eyebrow">YOUR LISTENING SPACE</span><h2>{output?.label ?? 'Choose an output device'}</h2><p>{output ? output.muted ? 'Output is muted' : `Output volume · ${volumeLabel(output.volume_percent)}` : 'No default output is selected'}</p></div><span class="audio-status" class:muted={output?.muted}><span></span>{output?.muted ? 'Muted' : output ? 'Connected' : 'No output'}</span></section>
  <section class="section" aria-labelledby="sound-output-title"><div class="section-heading"><div><h2 id="sound-output-title">Output</h2><p>Find the right place for music, meetings, and everything between.</p></div><span class="count">{snapshot.outputs.length} devices</span></div>
    <div class="settings-card">
      <div class="setting-row"><div><label for="sound-output">Output device</label><p>{output?.active_port ?? 'Choose where sound plays.'}</p></div><select id="sound-output" value={output?.id ?? ''} disabled={busy || !snapshot.outputs.length} onchange={event => { const device = event.currentTarget.value; event.currentTarget.value = output?.id ?? ''; void update({ type: 'set_default', device_id: device }); }}><option value="" disabled>{snapshot.outputs.length ? 'Choose an output' : 'No output devices'}</option>{#each snapshot.outputs as device}<option value={device.id}>{device.label}</option>{/each}</select></div>
      <div class="setting-row"><div><label for="sound-output-volume">Output volume</label><p>{output && output.volume_percent > 100 ? `Currently boosted to ${volumeLabel(output.volume_percent)}. Output boost above 100% may distort.` : 'A comfortable volume for your space.'}</p></div><div class="audio-volume"><input id="sound-output-volume" type="range" min="0" max="150" step="1" bind:value={outputVolume} disabled={busy || !output} aria-valuetext={volumeLabel(outputVolume)} oninput={() => editing = true} onchange={() => { if (output) void update({ type:'set_volume', device_id:output.id, percent:outputVolume }); }} /><output for="sound-output-volume">{volumeLabel(outputVolume)}</output></div></div>
      <div class="setting-row"><div><label for="sound-output-mute">Mute output</label><p>Pause the sound without changing your volume.</p></div><input id="sound-output-mute" class="switch" type="checkbox" role="switch" checked={output?.muted ?? false} disabled={busy || !output} onchange={event => { if (output) { const muted = event.currentTarget.checked; event.currentTarget.checked = output.muted; void update({ type:'set_mute', device_id:output.id, muted }); } }} /></div>
    </div>
    <div class="setting-row"><div><strong>Test speakers</strong><p>Play a quiet one-second tone on the selected output.</p></div>{#if speakerTesting}<button class="quiet" onclick={stopSpeaker}>Stop speaker test</button>{:else}<button class="quiet" disabled={busy || !output || output.muted} onclick={speakerTest}>Test speakers</button>{/if}</div>
    <p class="footnote">Applications can retain their own device selection when you change the default.</p>
  </section>
  <section class="section" aria-labelledby="sound-input-title"><div class="section-heading"><div><h2 id="sound-input-title">Input</h2><p>A clear voice starts with the right microphone.</p></div><span class="count">{snapshot.inputs.length} devices</span></div>
    <div class="settings-card">
      <div class="setting-row"><div><label for="sound-input">Input device</label><p>{input?.active_port ?? 'Choose the microphone you want to use.'}</p></div><select id="sound-input" value={input?.id ?? ''} disabled={busy || testing || !snapshot.inputs.length} onchange={event => { const device = event.currentTarget.value; event.currentTarget.value = input?.id ?? ''; void update({ type:'set_default', device_id:device }); }}><option value="" disabled>{snapshot.inputs.length ? 'Choose an input' : 'No microphones found'}</option>{#each snapshot.inputs as device}<option value={device.id}>{device.label}</option>{/each}</select></div>
      <div class="setting-row"><div><label for="sound-input-volume">Input volume</label><p>Adjust how strongly this microphone picks up your voice.</p></div><div class="audio-volume"><input id="sound-input-volume" type="range" min="0" max="100" step="1" bind:value={inputVolume} disabled={busy || !input} aria-valuetext={volumeLabel(inputVolume)} oninput={() => editing = true} onchange={() => { if (input) void update({ type:'set_volume', device_id:input.id, percent:inputVolume }); }} /><output for="sound-input-volume">{volumeLabel(inputVolume)}</output></div></div>
      <div class="setting-row"><div><label for="sound-input-mute">Mute microphone</label><p>{input?.muted ? 'This microphone is muted.' : 'Microphone tests only start when you choose Test microphone.'}</p></div><input id="sound-input-mute" class="switch" type="checkbox" role="switch" checked={input?.muted ?? false} disabled={busy || testing || !input} onchange={event => { if (input) { const muted = event.currentTarget.checked; event.currentTarget.checked = input.muted; void update({ type:'set_mute', device_id:input.id, muted }); } }} /></div>
    </div>
    <div class="microphone-test" class:recording={inputLevel?.active}><div class="meter-heading"><div><h2>Check your microphone</h2><p>{inputLevel?.active ? 'Speak naturally. You should see the level move.' : input?.muted ? 'Unmute your microphone to test its level.' : 'A quick level check before your next conversation.'}</p></div>{#if inputLevel?.active}<button class="quiet" onclick={stopTest}>Stop test</button>{:else}<button class="quiet" disabled={!input || input.muted || busy || testing} onclick={startTest}>{testing ? 'Starting…' : 'Test microphone'}</button>{/if}</div>
      <div class="meter-row"><span class="mic-symbol" aria-hidden="true">◉</span><meter id="microphone-level" min="0" max="100" low="15" high="90" optimum="60" value={level} aria-label="Microphone input level">{Math.round(level)}%</meter><span class="meter-status" role="status">{inputLevel?.active ? `${Math.ceil(inputLevel.remaining_ms / 1000)}s remaining` : 'Not listening'}</span></div>
      <p class="test-privacy"><span aria-hidden="true">◇</span>20-second level test. No recording is saved. Stops when you leave this page or window.</p>
    </div>
  </section>
  <div class="audio-footer"><span class="connection-dot"></span><span>{snapshot.server}</span><span class="immediate">Changes take effect immediately</span></div>
{/if}

<style>
  .audio-hero{display:flex;align-items:center;gap:18px;border:1px solid #e1def1;border-radius:var(--fd-radius);padding:25px;background:linear-gradient(125deg,#eeebfc,#fdfcff);margin-bottom:30px}.audio-hero-icon{height:56px;width:56px;flex-shrink:0;display:grid;place-items:center;color:var(--fd-accent);font-size:28px;background:white;border:1px solid #e4e0f6;border-radius:17px;box-shadow:0 5px 15px #635bdf0c}.audio-hero .eyebrow{display:block;font-size:8px;margin-bottom:8px}.audio-hero h2{font-size:18px}.audio-hero p{font-size:11px;color:var(--fd-text-muted);margin-top:8px}.audio-status{margin-left:auto;font-size:10px;color:var(--fd-success);white-space:nowrap;display:flex;align-items:center;gap:6px}.audio-status>span,.connection-dot{width:6px;height:6px;border-radius:50%;background:#65a48e;display:inline-block}.audio-status.muted{color:#9990a5}.audio-status.muted>span{background:#a49aaa}.settings-card{margin-top:0}.setting-row select{min-width:165px;max-width:290px;width:43%;padding:10px 12px;font-size:11px;border:1px solid var(--fd-border);border-radius:8px;background:var(--fd-surface);color:var(--fd-text);text-overflow:ellipsis}.audio-volume{display:flex;align-items:center;gap:16px;flex-shrink:0}.audio-volume input{width:160px;accent-color:var(--fd-accent)}.audio-volume output{min-width:35px;text-align:right;color:var(--fd-text-muted);font-size:11px}.microphone-test{padding:21px;margin-top:17px;border:1px solid var(--fd-border);border-radius:var(--fd-radius);background:var(--fd-surface)}.microphone-test.recording{border-color:#b9b1ee;box-shadow:0 0 0 3px #635bdf0b}.meter-heading{display:flex;gap:15px;align-items:center;justify-content:space-between}.meter-heading h2{font-size:13px}.meter-heading p{font-size:10px;color:var(--fd-text-muted);margin-top:7px;line-height:1.6}.meter-row{display:flex;gap:13px;align-items:center;margin:22px 0 18px}.mic-symbol{color:#9189bd;font-size:19px}.meter-row meter{height:12px;flex:1;width:100%;border:0;background:#eff0f7;border-radius:8px;overflow:hidden}.meter-row meter::-webkit-meter-bar{background:#eff0f7;border:0;border-radius:8px}.meter-row meter::-webkit-meter-optimum-value{background:linear-gradient(90deg,#b5a8e8,#6c5ed6);border-radius:8px}.meter-row meter::-webkit-meter-suboptimum-value{background:#b1a4e2;border-radius:8px}.meter-row meter::-webkit-meter-even-less-good-value{background:#d58f76;border-radius:8px}.meter-status{font-size:9px;min-width:74px;text-align:right;color:#9299ab}.test-privacy{display:flex;gap:7px;align-items:center;color:#9299ab;font-size:9px;line-height:1.8}.audio-footer{display:flex;align-items:center;gap:7px;font-size:9px;line-height:1.7;color:#9299ab;margin-top:23px}.audio-footer .immediate{margin-left:auto}.connection-dot{width:5px;height:5px}@media(max-width:950px){.audio-hero{padding:20px;gap:12px}.audio-hero h2{font-size:15px}.audio-hero-icon{height:45px;width:45px}.audio-status{font-size:9px}.audio-volume{gap:8px}.audio-volume input{width:100px}.setting-row select{min-width:145px}.setting-row>div:first-child{max-width:55%}.test-privacy{align-items:flex-start}}@media(max-width:600px){.audio-hero{flex-wrap:wrap}.audio-status{margin-left:0}.setting-row select{width:100%;max-width:none}.setting-row>div:first-child{max-width:none}.audio-volume{width:100%}.audio-volume input{flex:1}.meter-heading{align-items:flex-start;flex-direction:column}.audio-footer{flex-wrap:wrap}.audio-footer .immediate{margin-left:0;width:100%}}
</style>
