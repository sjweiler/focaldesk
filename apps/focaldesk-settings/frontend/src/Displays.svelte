<script lang="ts">
  import { onMount } from 'svelte';
  import DisplayLayoutPreferences from './DisplayLayoutPreferences.svelte';
  import HdrCalibration from './HdrCalibration.svelte';
  import ExclusiveHdr from './ExclusiveHdr.svelte';
  import {invoke} from '@tauri-apps/api/core';
  import { getDisplays, previewDisplays, confirmDisplays, revertDisplays, openExistingSettings } from './api';
  import type { DisplayConfiguration, OutputConfig, DisplayTransform } from './api';
  import { logicalSize, layoutBounds, snapPosition } from './display-layout';

  let configuration = $state<DisplayConfiguration | null>(null);
  let saved = $state<OutputConfig[]>([]);
  let draft = $state<OutputConfig[]>([]);
  let selectedConnector = $state('');
  let loading = $state(true);
  let busy = $state(false);
  let polling = false;
  let error = $state('');
  let loadError = $state('');
  let message = $state('');
  let boardWidth = $state(700);
  let drag = $state<{ connector: string; clientX: number; clientY: number; x: number; y: number; factor: number; bounds: ReturnType<typeof layoutBounds> } | null>(null);
  let dirty = $derived(JSON.stringify(saved) !== JSON.stringify(draft));
  let trial = $derived(configuration?.preview);
  let selected = $derived(draft.find(output => output.connector === selectedConnector));
  let capability = $derived(configuration?.outputs.find(output => output.config.connector === selectedConnector));
  let bounds = $derived(drag?.bounds ?? layoutBounds(draft));
  let fit = $derived(Math.max(0.001, Math.min((boardWidth - 64) / bounds.width, 205 / bounds.height, 0.22)));
  let offsetX = $derived((boardWidth - bounds.width * fit) / 2);
  let offsetY = $derived((270 - bounds.height * fit) / 2);
  let valid = $derived(draft.every(output => [output.x, output.y, output.scale, output.width, output.height, output.refresh_mhz].every(Number.isFinite)));
  let resolutions = $derived([...new Set(capability?.modes.map(mode => `${mode.width}×${mode.height}`) ?? [])]);
  let refreshRates = $derived(capability?.modes.filter(mode => mode.width === selected?.width && mode.height === selected?.height) ?? []);
  const transforms: [DisplayTransform, string][] = [['Normal', 'Landscape'], ['Rotate90', 'Portrait right'], ['Rotate180', 'Landscape flipped'], ['Rotate270', 'Portrait left']];

  function copy<T>(value: T): T { return JSON.parse(JSON.stringify(value)); }
  function accept(next: DisplayConfiguration, replace = false) {
    const finished = !!configuration?.preview && !next.preview;
    configuration = next;
    if (replace || finished || (!dirty && !next.preview)) {
      saved = next.outputs.map(output => copy(output.config));
      draft = copy(saved);
      if (!draft.some(output => output.connector === selectedConnector)) selectedConnector = draft[0]?.connector ?? '';
    }
    if (finished) message = 'Display configuration refreshed. Unconfirmed trials are automatically restored.';
  }
  async function refresh(replace = false) {
    if (polling || busy) return;
    polling = true;
    try { accept(await getDisplays(), replace); loadError = ''; if (replace) error = ''; }
    catch (cause) { loadError = String(cause); }
    finally { polling = false; loading = false; }
  }
  async function testChanges() {
    if (busy || trial || !valid) return;
    busy = true; error = ''; message = '';
    try { accept(await previewDisplays(copy(saved), copy(draft))); }
    catch (cause) { error = String(cause); }
    finally { busy = false; }
  }
  async function finish(keep: boolean) {
    if (!trial || busy) return;
    const id = trial.id;
    busy = true; error = ''; message = '';
    try {
      accept(await (keep ? confirmDisplays(id) : revertDisplays(id)), keep);
      message = keep ? 'Your display settings are saved.' : 'Restoring the previous display configuration…';
    } catch (cause) { error = String(cause); }
    finally { busy = false; }
  }
  function discard() { draft = copy(saved); error = ''; message = ''; }
  function resolution(value: string) {
    if (!selected || !capability) return;
    const [width, height] = value.split('×').map(Number);
    const modes = capability.modes.filter(mode => mode.width === width && mode.height === height);
    const mode = modes.find(mode => mode.refresh_mhz === selected?.refresh_mhz) ?? modes.at(-1);
    if (mode) { selected.width = mode.width; selected.height = mode.height; selected.refresh_mhz = mode.refresh_mhz; }
  }
  function enable(value: boolean) {
    if(!selected)return;
    selected.enabled=value;
    if(!value)selected.primary=false;
    if(!draft.some(output=>output.enabled&&output.role==='Normal'&&output.primary)){const fallback=draft.find(output=>output.enabled&&output.role==='Normal');if(fallback)fallback.primary=true;}
  }
  function role(value: 'Normal'|'ControlPanel') {
    if(!selected)return;selected.role=value;if(value==='ControlPanel')selected.primary=false;
    if(!draft.some(output=>output.enabled&&output.role==='Normal'&&output.primary)){const fallback=draft.find(output=>output.enabled&&output.role==='Normal');if(fallback)fallback.primary=true;}
  }
  function primary() { for (const output of draft) output.primary = output.connector === selectedConnector; }
  function move(event: PointerEvent) {
    if (!drag || trial || busy) return;
    const output = draft.find(output => output.connector === drag?.connector);
    if (!output) return;
    Object.assign(output, snapPosition(output, draft.filter(other => other !== output), drag.x + (event.clientX - drag.clientX) / drag.factor, drag.y + (event.clientY - drag.clientY) / drag.factor));
  }
  function keyboard(event: KeyboardEvent, output: OutputConfig) {
    if (trial || busy) return;
    const delta: Record<string, [number, number]> = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] };
    if (!delta[event.key]) return;
    event.preventDefault();
    const [dx, dy] = delta[event.key]; const step = event.shiftKey ? 50 : 10;
    output.x += dx * step; output.y += dy * step;
  }
  async function chooseIcc(){if(!selected||trial||busy)return;const connector=selected.connector;busy=true;error='';try{const path=await invoke<string|null>('pick_display_icc');if(path){const output=draft.find(output=>output.connector===connector);if(output)output.icc_profile_path=path;}}catch(cause){error=String(cause);}finally{busy=false;}}
  async function advanced() {
    try { await openExistingSettings('displays'); }
    catch (cause) { error = String(cause); }
  }
  onMount(() => {
    void refresh(true);
    const timer = window.setInterval(() => void refresh(), 1000);
    return () => window.clearInterval(timer);
  });
</script>

<div class="page-heading"><div><div class="eyebrow">A PLACE FOR EVERY WINDOW</div><h1>Displays</h1><p>Arrange your space. Find the right balance of detail and comfort.</p></div><button class="quiet" disabled={!!trial || busy} onclick={advanced}>Exclusive HDR restart ↗</button></div>
{#if error || loadError}<div class="notice error" role="alert"><span>{error || loadError}</span><button disabled={busy || !!trial} onclick={() => refresh(true)}>Reload</button></div>{/if}
{#if message}<div class="notice success" role="status">{message}</div>{/if}
{#if trial}
  <div class="display-confirm" role="alertdialog" tabindex="-1" aria-labelledby="confirm-displays-title" aria-describedby="confirm-displays-detail">
    <div><h2 id="confirm-displays-title">{trial.phase === 'reverting' ? 'Restoring your previous setup' : trial.phase === 'applying' ? 'Testing your display setup' : 'Keep this display setup?'}</h2><p id="confirm-displays-detail">{trial.phase === 'reverting' ? 'The compositor is restoring the previous configuration.' : `Automatically restores in ${Math.ceil(trial.remaining_ms / 1000)} seconds, even if Settings closes.`}</p></div>
    <div class="confirm-actions"><button class="quiet" disabled={busy || trial.phase === 'reverting'} onclick={() => finish(false)}>Revert now</button><button class="primary" disabled={busy || trial.phase !== 'confirming' || trial.remaining_ms === 0} onclick={() => finish(true)}>Keep changes</button></div>
  </div>
{/if}
{#if loading}<div class="loading" role="status">Reading connected displays…</div>
{:else if !configuration}<div class="loading" role="status">Display information could not be loaded. Your desktop session may still be running.</div>
{:else if !draft.length}<div class="loading">No active displays are available. Start a FocalDesk session, then reload.<p><button class="quiet" onclick={() => refresh(true)}>Reload displays</button></p></div>
{:else}
  <section class="display-arrangement" aria-labelledby="arrangement-title"><div class="section-heading"><div><h2 id="arrangement-title">Your workspace, connected</h2><p>Drag displays to arrange them. Arrow keys move a selected display; Shift moves faster.</p></div><span class="count">{draft.length} connected</span></div>
    <div class="monitor-board" bind:clientWidth={boardWidth}>
      {#each draft as output, index (output.connector)}
        {@const size = logicalSize(output)}
        <button class="monitor" class:chosen={selectedConnector === output.connector} class:control-panel={output.role === 'ControlPanel'} class:inactive={!output.enabled} disabled={!!trial || busy} aria-pressed={selectedConnector === output.connector} aria-label={`${output.connector}, display ${index + 1}${output.primary ? ', primary' : ''}${output.enabled ? '' : ', disabled'}. Use arrow keys to move.`}
          style={`left:${offsetX + (output.x - bounds.x) * fit}px;top:${offsetY + (output.y - bounds.y) * fit}px;width:${size.width * fit}px;height:${size.height * fit}px`}
          onclick={() => selectedConnector = output.connector}
          onkeydown={event => keyboard(event, output)}
          onpointerdown={event => { if (trial || busy) return; selectedConnector = output.connector; event.currentTarget.setPointerCapture(event.pointerId); drag = {connector: output.connector, clientX: event.clientX, clientY: event.clientY, x: output.x, y: output.y, factor: fit, bounds: { ...bounds }}; }}
          onpointermove={move} onpointerup={() => drag = null} onpointercancel={() => drag = null}>
          <span class="monitor-number">{index + 1}</span><span class="monitor-name">{output.connector}</span>{#if output.primary}<span class="monitor-primary">PRIMARY</span>{/if}
        </button>
      {/each}
      <span class="board-hint">Logical desktop · edges snap together</span>
    </div>
    <div class="monitor-tabs" aria-label="Select display">{#each draft as output, index}<button class:active={selectedConnector === output.connector} aria-pressed={selectedConnector === output.connector} onclick={() => selectedConnector = output.connector}>{index + 1} <span>{output.connector}</span></button>{/each}</div>
  </section>
  {#if selected && capability}
    <section class="section" aria-labelledby="selected-display-title"><div class="section-heading"><div><h2 id="selected-display-title">{capability.make} {capability.model || selected.connector}</h2><p>{selected.connector} · {!selected.enabled ? 'Disabled display' : selected.role === 'ControlPanel' ? 'Reserved control panel' : selected.primary ? 'Primary display' : 'Extended display'}</p></div><span class="display-badge">{capability.hdr_active ? 'HDR active' : 'SDR active'}</span></div>
      <fieldset disabled={!!trial || busy} class="display-fields"><legend class="sr-only">Display configuration</legend>
        <div class="settings-card">
          <div class="setting-row"><div><label for="display-enabled">Enable display</label><p>Connected disabled displays stay available here for re-enabling.</p></div><input id="display-enabled" class="switch" type="checkbox" role="switch" disabled={!configuration?.hardware_modes} checked={selected.enabled} onchange={event=>enable(event.currentTarget.checked)} /></div>
          <div class="setting-row"><div><label for="display-role">Display role</label><p>Control Panel reserves this display for companion controls. Keep another desktop display enabled.</p></div><select id="display-role" value={selected.role} onchange={event=>role(event.currentTarget.value as 'Normal'|'ControlPanel')}><option value="Normal">Desktop</option><option value="ControlPanel">Control Panel</option></select></div>
          <div class="setting-row"><div><label for="display-resolution">Resolution</label><p>Modes advertised by this display.</p></div><select id="display-resolution" value={`${selected.width}×${selected.height}`} onchange={event => resolution(event.currentTarget.value)}>{#each resolutions as value}<option value={value}>{value}</option>{/each}</select></div>
          <div class="setting-row"><div><label for="display-refresh">Refresh rate</label><p>Smoother motion, at the selected resolution.</p></div><select id="display-refresh" bind:value={selected.refresh_mhz}>{#each refreshRates as mode}<option value={mode.refresh_mhz}>{(mode.refresh_mhz / 1000).toLocaleString(undefined, { maximumFractionDigits: 3 })} Hz</option>{/each}</select></div>
          <div class="setting-row"><div><label for="display-scale">Scale</label><p>Adjust the size of text, controls, and windows.</p></div><select id="display-scale" bind:value={selected.scale}>{#each [...new Set([1, 1.25, 1.3333334, 1.5, 1.75, 2, 2.5, 3, 4, selected.scale])].sort((a,b) => a-b) as scale}<option value={scale}>{Math.round(scale * 100)}%</option>{/each}</select></div>
          <div class="setting-row"><div><label for="display-orientation">Orientation</label><p>Match the way your monitor is positioned.</p></div><select id="display-orientation" bind:value={selected.transform}>{#each transforms as [value, name]}<option value={value}>{name}</option>{/each}</select></div>
          <div class="setting-row"><div><span>Desktop position</span><p>Coordinates are in logical pixels.</p></div><div class="position-controls"><label for="display-x">X</label><input id="display-x" type="number" min="-32768" max="32768" step="1" bind:value={selected.x} /><label for="display-y">Y</label><input id="display-y" type="number" min="-32768" max="32768" step="1" bind:value={selected.y} /></div></div>
          <div class="setting-row"><div><span>Primary display</span><p>Use this display as the desktop's main destination.</p></div><button class="quiet" disabled={!selected.enabled || selected.primary || selected.role !== 'Normal'} onclick={primary}>{selected.primary ? 'Primary display' : 'Use as primary'}</button></div>
        </div>
        <div class="section-heading color-heading"><div><h2>Color & dynamic range</h2><p>Capability and active configuration are reported by the compositor.</p></div></div>
        <div class="settings-card">
          <div class="setting-row"><div><label for="display-hdr">High dynamic range</label><p>{capability.hdr_supported ? capability.hdr_active ? 'HDR is verified active.' : 'HDR capable · currently rendering SDR.' : 'This display does not advertise HDR support.'}</p></div><input class="switch" id="display-hdr" type="checkbox" role="switch" disabled={!capability.hdr_supported || !configuration?.hardware_modes} bind:checked={selected.hdr_requested} /></div>
          <div class="setting-row"><div><label for="display-color">Color profile</label><p>{selected.icc_profile_path ? 'An ICC profile is assigned. Change or clear it below.' : capability.wide_gamut_active ? 'Wide-gamut rendering is active.' : 'Standard-gamut rendering is active.'}</p></div><select id="display-color" bind:value={selected.color_profile}><option value="auto">Automatic</option><option value="srgb">sRGB</option><option value="display_p3">Display P3</option></select></div>
          <div class="setting-row"><div><label for="display-icc">ICC profile file</label><p>Absolute local RGB display profile, validated by the compositor during Test changes.</p></div><input id="display-icc" class="icc-path" type="text" maxlength="4096" value={selected.icc_profile_path??''} oninput={event=>{if(selected)selected.icc_profile_path=event.currentTarget.value||null;}} /><button class="quiet" onclick={chooseIcc}>Choose ICC…</button><button class="quiet" disabled={!selected.icc_profile_path} onclick={()=>{if(selected)selected.icc_profile_path=null;}}>Clear ICC</button></div>
        </div>
      </fieldset>
      {#if capability.hdr_supported}{#key selected.connector}<HdrCalibration bind:appearance={selected.hdr_appearance} connector={selected.connector} editable={!!configuration?.hardware_modes&&selected.enabled&&selected.hdr_requested} locked={!!trial||busy} {trial} hdrActive={capability.hdr_active} />{/key}{/if}
      {#if capability.icc_lut_fallback_active}<div class="notice display-warning" role="status">The compositor is using its ICC LUT fallback for this display.</div>{/if}
      {#if !configuration?.hardware_modes}<p class="footnote">Nested session: hardware modes and HDR are controlled by the host compositor.</p>{/if}
      {#if capability.hdr_supported && configuration?.hardware_modes}{#key selected.connector}<ExclusiveHdr connector={selected.connector} disabled={!!trial || busy || dirty} />{/key}{/if}
      <p class="footnote">Changes are tested for 20 seconds. Only Keep changes saves them. Keep an enabled primary desktop display. ICC and HDR appearance use this same trial. </p>
    </section>
  {/if}
{/if}
<DisplayLayoutPreferences disabled={!!trial||busy||dirty} />
<footer class="apply-bar"><div><span class="status-dot" class:unsaved={dirty || !!trial}></span>{trial ? 'Compositor recovery is active' : dirty ? 'You have untested display changes' : 'Your displays are up to date'}</div><div class="apply-actions"><button class="quiet" disabled={!dirty || busy || !!trial} onclick={discard}>Discard</button><button class="primary" disabled={!dirty || busy || loading || !!trial || !!loadError || !valid} onclick={testChanges}>{busy ? 'Working…' : 'Test changes'}</button></div></footer>

<style>
  .icc-path{max-width:270px;min-width:0;padding:9px;border:1px solid var(--fd-border);border-radius:8px;background:var(--fd-surface);color:var(--fd-text);font:inherit;font-size:11px}
  .display-arrangement{background:var(--fd-surface);border:1px solid var(--fd-border);border-radius:var(--fd-radius);padding:20px;margin-top:24px}.monitor-board{height:270px;position:relative;overflow:hidden;border:1px solid var(--fd-border);border-radius:12px;background-color:#f6f7fc;background-image:radial-gradient(#cfd3e3 .7px,transparent .7px);background-size:12px 12px}.monitor{position:absolute;border:2px solid #b9b5d9;border-radius:8px;background:linear-gradient(135deg,#b8b4e6,#e7ddf0);color:#4d4775;display:flex;align-items:center;justify-content:center;flex-direction:column;gap:5px;box-shadow:0 5px 15px #30255a14;touch-action:none;user-select:none;cursor:grab;min-width:0;overflow:hidden;padding:4px}.monitor:active{cursor:grabbing}.monitor.chosen{border-color:var(--fd-accent);background:linear-gradient(135deg,#766bdc,#b3a0db);color:white;box-shadow:0 0 0 3px #635bdf22}.monitor.control-panel{border-style:dashed}.monitor.inactive{opacity:.45;border-style:dotted}.monitor-number{font-size:24px;font-weight:600}.monitor-name{font-size:10px}.monitor-primary{font-size:7px;letter-spacing:1.5px;margin-top:5px;opacity:.7}.board-hint{position:absolute;bottom:10px;left:12px;color:#8f96ab;font-size:9px;pointer-events:none}.monitor-tabs{display:flex;gap:8px;flex-wrap:wrap;margin-top:15px}.monitor-tabs button{padding:8px 12px;border-radius:7px;color:#777d91;background:#f2f3f8;font-size:11px}.monitor-tabs button.active{background:var(--fd-accent-soft);color:var(--fd-accent)}.monitor-tabs span{margin-left:6px}.display-fields{padding:0;border:0;margin:0;min-width:0}.display-fields .settings-card{margin-top:0}.display-fields select{max-width:230px;min-width:145px;padding:9px 11px;border:1px solid var(--fd-border);border-radius:8px;background:var(--fd-surface);color:var(--fd-text);font-size:11px}.position-controls{display:flex;gap:7px;align-items:center;color:var(--fd-text-muted);font-size:10px}.position-controls input{width:77px;padding:8px 5px;border:1px solid var(--fd-border);border-radius:7px;color:var(--fd-text);background:var(--fd-surface);font-size:11px}.display-badge{font-size:10px;color:var(--fd-success);background:#eaf6ef;padding:6px 10px;border-radius:6px;white-space:nowrap}.color-heading{margin-top:27px}.display-confirm{position:sticky;top:12px;z-index:6;display:flex;gap:18px;align-items:center;justify-content:space-between;background:#eeecff;border:1px solid #bcb4ec;border-radius:12px;padding:20px;margin-bottom:22px;box-shadow:0 8px 30px #362f6d1a}.display-confirm h2{font-size:15px;color:#4b427f}.display-confirm p{font-size:11px;color:#746a9c;line-height:1.7;margin-top:7px}.confirm-actions{display:flex;gap:8px;flex-shrink:0}.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}.display-warning{margin-top:15px;background:#fff8e9;color:#8a652b;border:1px solid #ebd7b2}@media(max-width:1000px){.display-confirm{align-items:flex-start;flex-direction:column}.display-arrangement{padding:15px}.display-arrangement .section-heading{gap:12px}.display-arrangement .section-heading p{line-height:1.7}.position-controls input{width:65px}}@media(prefers-reduced-motion:reduce){.monitor{transition:none}}
</style>
