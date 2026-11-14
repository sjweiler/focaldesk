<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { copy } from './input-preferences';
  import { clockTime, clockMinutes } from './accessibility';
  import type { AccessibilityDraft, AccessibilitySnapshot } from './accessibility';
  let saved = $state<AccessibilityDraft | null>(null), draft = $state<AccessibilityDraft | null>(null);
  let configuredOutputs = $state<string[]>([]);
  let busy = $state(false), loading = $state(true), error = $state(''), message = $state('');
  let dirty = $derived(!!saved && !!draft && JSON.stringify(saved)!==JSON.stringify(draft));
  let outputs = $derived([...new Set([...configuredOutputs,...Object.keys(draft?.visual.display_comfort_disabled_outputs ?? {}),...Object.keys(draft?.visual.magnifier_output_zoom ?? {})])]);
  const keyboardToggles = [
    ['sticky_keys','Sticky Keys','Use modifier keys one at a time instead of holding them.'],
    ['sticky_keys_shortcut','Sticky Keys shortcut','Ask before toggling Sticky Keys when Shift is pressed five times.'],
    ['visual_feedback','Visual feedback','Show a brief notification for toggles and latched modifiers.'],
    ['slow_keys','Slow Keys','Accept a key only after it has been held for the configured delay.'],
    ['bounce_keys','Bounce Keys','Ignore repeated presses of the same key for a short interval.'],
  ] as const;
  async function reload() {
    busy=true; error=''; message='';
    try { const next=await invoke<AccessibilitySnapshot>('get_accessibility'); saved=copy(next.preferences); draft=copy(next.preferences); configuredOutputs=next.outputs; }
    catch (cause) { error=String(cause); } finally { busy=false; loading=false; }
  }
  async function apply() {
    if (!saved || !draft) return;
    busy=true; error=''; message='';
    try { const result=await invoke<{ snapshot: AccessibilitySnapshot; message: string }>('apply_accessibility',{ expected: copy(saved), requested: copy(draft) }); saved=copy(result.snapshot.preferences); draft=copy(result.snapshot.preferences); configuredOutputs=result.snapshot.outputs; message=result.message; }
    catch (cause) { error=String(cause); } finally { busy=false; }
  }
  function resetColor() { if (draft) { draft.visual.night_light.enabled=false; draft.visual.color_filter='off'; draft.visual.display_comfort_disabled_outputs={}; } }
  function setTime(key: 'custom_start_minutes' | 'custom_end_minutes', value: string) { const minutes=clockMinutes(value); if (draft && minutes!==null) draft.visual.night_light[key]=minutes; }
  onMount(() => { void reload(); });
</script>

<div class="page-heading"><div><div class="eyebrow">A DESKTOP THAT WORKS FOR YOU</div><h1>Accessibility</h1><p>Make typing, seeing, and navigating more comfortable.</p></div><button class="quiet" disabled={busy} onclick={reload}>Reload</button></div>
{#if error}<div class="notice error" role="alert">{error}</div>{/if}
{#if message}<div class="notice success" role="status">{message}</div>{/if}
{#if loading}<div class="loading" role="status">Reading accessibility preferences…</div>
{:else if draft}
<fieldset disabled={busy} class="accessibility-fields"><legend class="sr-only">Accessibility preferences</legend>
  <section class="section"><h2>Keyboard</h2><p class="description">Draft changes take effect after Apply. Keyboard filters affect every application.</p><div class="settings-card">
    {#each keyboardToggles as [key,label,description]}<div class="setting-row"><div><label for={`accessibility-${key}`}>{label}</label><p>{description}</p></div><input class="switch" id={`accessibility-${key}`} type="checkbox" role="switch" bind:checked={draft.keyboard[key]} /></div>{/each}
    <div class="setting-row"><div><label for="accessibility-slow-delay">Slow Keys delay</label><p>How long to hold a key before it is accepted.</p></div><div class="range"><input id="accessibility-slow-delay" type="range" min="100" max="2000" step="50" disabled={!draft.keyboard.slow_keys} bind:value={draft.keyboard.slow_keys_delay_ms} aria-valuetext={`${draft.keyboard.slow_keys_delay_ms} milliseconds`} /><output for="accessibility-slow-delay">{draft.keyboard.slow_keys_delay_ms} ms</output></div></div>
    <div class="setting-row"><div><label for="accessibility-bounce-delay">Bounce Keys interval</label><p>Ignore repeated presses during this interval.</p></div><div class="range"><input id="accessibility-bounce-delay" type="range" min="100" max="2000" step="50" disabled={!draft.keyboard.bounce_keys} bind:value={draft.keyboard.bounce_keys_delay_ms} aria-valuetext={`${draft.keyboard.bounce_keys_delay_ms} milliseconds`} /><output for="accessibility-bounce-delay">{draft.keyboard.bounce_keys_delay_ms} ms</output></div></div>
  </div></section>
  <section class="section"><h2>Vision and motion</h2><div class="settings-card">
    <div class="setting-row"><div><label for="accessibility-magnifier">Screen magnifier</label><p>Magnify each display around its current focus point.</p></div><input id="accessibility-magnifier" class="switch" type="checkbox" role="switch" bind:checked={draft.visual.magnifier_enabled} /></div>
    <div class="setting-row"><div><label for="accessibility-zoom">Magnification</label><p>Changing this value resets saved per-display zoom overrides.</p></div><div class="range"><input id="accessibility-zoom" type="range" min="1" max="8" step="0.25" disabled={!draft.visual.magnifier_enabled} bind:value={draft.visual.magnifier_zoom} oninput={() => { if (draft) draft.visual.magnifier_output_zoom={}; }} aria-valuetext={`${draft.visual.magnifier_zoom} times`} /><output for="accessibility-zoom">{draft.visual.magnifier_zoom.toFixed(2)}×</output></div></div>
    <div class="setting-row"><div><label for="accessibility-follow-focus">Follow keyboard focus</label><p>Move the magnified view to the focused window.</p></div><input id="accessibility-follow-focus" class="switch" type="checkbox" role="switch" bind:checked={draft.visual.magnifier_follow_focus} /></div>
    <div class="setting-row"><div><label for="accessibility-cursor">Pointer size</label><p>Increase the pointer size across the desktop.</p></div><div class="range"><input id="accessibility-cursor" type="range" min="24" max="96" step="8" bind:value={draft.visual.cursor_size} aria-valuetext={`${draft.visual.cursor_size} pixels`} /><output for="accessibility-cursor">{draft.visual.cursor_size} px</output></div></div>
  </div><p class="footnote">Magnifier shortcuts: Super++ / Super+−.</p></section>
  <section class="section"><h2>On-screen keyboard</h2><div class="settings-card"><div class="setting-row"><div><label for="accessibility-osk">Enable on-screen keyboard</label><p>Starts the trusted Maliit keyboard after Apply. Requires Maliit to be installed; it shows and hides when text fields request input.</p></div><input id="accessibility-osk" class="switch" type="checkbox" role="switch" bind:checked={draft.visual.on_screen_keyboard_enabled} /></div></div></section>
  <section class="section"><h2>Color and light</h2><p class="description">Display-only adjustments. Screen sharing and screenshots keep application colors unchanged.</p><div class="settings-card">
    <div class="setting-row"><div><label for="accessibility-night-light">Night Light</label><p>Warm the display to reduce blue light.</p></div><input id="accessibility-night-light" class="switch" type="checkbox" role="switch" bind:checked={draft.visual.night_light.enabled} /></div>
    <div class="setting-row"><div><label for="accessibility-temperature">Color temperature</label><p>Lower values produce a warmer display.</p></div><div class="range"><input id="accessibility-temperature" type="range" min="1000" max="6500" step="100" disabled={!draft.visual.night_light.enabled} bind:value={draft.visual.night_light.temperature_kelvin} aria-valuetext={`${draft.visual.night_light.temperature_kelvin} kelvin`} /><output for="accessibility-temperature">{draft.visual.night_light.temperature_kelvin} K</output></div></div>
    <div class="setting-row"><div><label for="accessibility-schedule">Night Light schedule</label><p>Solar scheduling uses your coordinates locally, without a network lookup.</p></div><select id="accessibility-schedule" bind:value={draft.visual.night_light.schedule}><option value="manual">Manual</option><option value="custom">Custom hours</option><option value="automatic">Sunset to sunrise</option></select></div>
    {#if draft.visual.night_light.schedule!=='manual'}
    <div class="setting-row"><div><label for="accessibility-start">Start time</label><p>Custom hours are also the fallback when solar coordinates are missing.</p></div><input id="accessibility-start" type="time" required value={clockTime(draft.visual.night_light.custom_start_minutes)} oninput={event => setTime('custom_start_minutes',event.currentTarget.value)} /></div>
    <div class="setting-row"><label for="accessibility-end">End time</label><input id="accessibility-end" type="time" required value={clockTime(draft.visual.night_light.custom_end_minutes)} oninput={event => setTime('custom_end_minutes',event.currentTarget.value)} /></div>
    {/if}
    {#if draft.visual.night_light.schedule==='automatic'}
    <div class="setting-row"><div><label for="accessibility-latitude">Solar schedule latitude</label><p>Decimal degrees; positive is north. Leave blank if unknown.</p></div><input id="accessibility-latitude" type="number" min="-90" max="90" step="0.0001" value={draft.visual.night_light.latitude ?? ''} oninput={event => { if (draft) draft.visual.night_light.latitude=event.currentTarget.value==='' ? null : event.currentTarget.valueAsNumber; }} /></div>
    <div class="setting-row"><div><label for="accessibility-longitude">Solar schedule longitude</label><p>Decimal degrees; negative is west. Leave blank if unknown.</p></div><input id="accessibility-longitude" type="number" min="-180" max="180" step="0.0001" value={draft.visual.night_light.longitude ?? ''} oninput={event => { if (draft) draft.visual.night_light.longitude=event.currentTarget.value==='' ? null : event.currentTarget.valueAsNumber; }} /></div>
    {/if}
    <div class="setting-row"><div><label for="accessibility-transition">Night Light transition</label><p>Warm-up and cool-down around schedule boundaries.</p></div><div class="range"><input id="accessibility-transition" type="range" min="0" max="60" step="5" bind:value={draft.visual.night_light.transition_minutes} /><output for="accessibility-transition">{draft.visual.night_light.transition_minutes} min</output></div></div>
    <div class="setting-row"><div><label for="accessibility-filter">Color filter</label><p>Color-vision aids are applied after monitor calibration.</p></div><select id="accessibility-filter" bind:value={draft.visual.color_filter}><option value="off">Off</option><option value="grayscale">Grayscale</option><option value="protanopia">Protanopia</option><option value="deuteranopia">Deuteranopia</option><option value="tritanopia">Tritanopia</option></select></div>
  </div></section>
  <section class="section"><h2>Ambient light adaptation</h2><div class="settings-card">
    <div class="setting-row"><div><label for="accessibility-ambient">Adapt display brightness</label><p>Uses a local IIO light sensor. Unsupported or permission-denied sensors leave the display unchanged.</p></div><input id="accessibility-ambient" class="switch" type="checkbox" role="switch" bind:checked={draft.visual.ambient_light_enabled} /></div>
    <div class="setting-row"><div><label for="accessibility-brightness">Minimum brightness</label><p>Lower limit for automatic adaptation.</p></div><div class="range"><input id="accessibility-brightness" type="range" min="0.35" max="1" step="0.05" disabled={!draft.visual.ambient_light_enabled} bind:value={draft.visual.ambient_min_brightness} /><output for="accessibility-brightness">{Math.round(draft.visual.ambient_min_brightness*100)}%</output></div></div>
  </div></section>
  <section class="section"><h2>Color and contrast reference</h2><div class="reference-card"><div class="plates" aria-label="Red, green, blue, yellow, magenta, and cyan reference colors">{#each ['#f00','#0f0','#00f','#ff0','#f0f','#0ff'] as color}<span style:background={color}></span>{/each}</div><div class="plates" aria-label="White to black neutral ramp">{#each ['#fff','#bfbfbf','#808080','#404040','#000'] as color}<span style:background={color}></span>{/each}</div><p>Black on white is 21:1. Keep important text at 4.5:1 or higher. These are reference colors; draft filters apply after you save.</p><button class="quiet" onclick={resetColor}>Reset color comfort</button></div><p class="footnote">Toggle Night Light: Super+Shift+N. Cycle color filters: Super+Shift+C.</p></section>
  <section class="section"><h2>Display comfort by monitor</h2><p class="description">Exclude Night Light and color filters on individual configured monitors.</p><div class="settings-card">
    {#each outputs as connector}<div class="setting-row"><div><label for={`comfort-${connector}`}>{connector}</label><p>{configuredOutputs.includes(connector) ? 'Apply Night Light and color filters to this display.' : 'Saved override for a display outside the current configured list.'}</p>{#if draft.visual.magnifier_output_zoom[connector]}<p>Saved magnifier zoom: {draft.visual.magnifier_output_zoom[connector]}×</p>{/if}</div><input id={`comfort-${connector}`} class="switch" type="checkbox" role="switch" checked={!draft.visual.display_comfort_disabled_outputs[connector]} onchange={event => { if (draft) { if (event.currentTarget.checked) delete draft.visual.display_comfort_disabled_outputs[connector]; else draft.visual.display_comfort_disabled_outputs[connector]=true; } }} /></div>{:else}<div class="setting-row"><p>No configured displays. Connect a display and configure it in Displays.</p></div>{/each}
  </div></section>
</fieldset>
{/if}
<footer class="apply-bar"><div><span class="status-dot" class:unsaved={dirty}></span>{dirty ? 'You have unapplied accessibility changes' : 'Preferences match the saved configuration'}</div><div class="apply-actions"><button class="quiet" disabled={!dirty || busy} onclick={() => { if (saved) draft=copy(saved); error=''; message=''; }}>Discard</button><button class="primary" disabled={!dirty || busy || loading} onclick={apply}>{busy ? 'Applying…' : 'Apply changes'}</button></div></footer>

<style>
  .accessibility-fields{border:0;padding:0;margin:0;min-width:0}.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}.description{color:var(--fd-text-muted);font-size:12px;line-height:1.8;margin:10px 0 15px}.range{display:flex;align-items:center;gap:12px}.range input{width:130px;accent-color:var(--fd-accent)}output{font-size:12px;color:var(--fd-text-muted);min-width:62px;text-align:right}select,input[type=time],input[type=number]{padding:10px;border:1px solid var(--fd-border);border-radius:8px;background:var(--fd-surface);color:var(--fd-text);font:inherit;font-size:12px;max-width:200px}.reference-card{border:1px solid var(--fd-border);border-radius:var(--fd-radius);padding:20px;background:var(--fd-surface)}.plates{display:flex;margin-bottom:12px;border:1px solid var(--fd-border);border-radius:6px;overflow:hidden}.plates span{height:32px;flex:1}.reference-card p{font-size:12px;color:var(--fd-text-muted);line-height:1.8;margin:15px 0}.footnote{line-height:1.8}@media(max-width:900px){.range input{width:90px}}@media(max-width:600px){.range{width:100%}.range input{flex:1}}
</style>
