<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { getInputConfiguration, applyInputConfiguration } from './api';
  import type { InputConfiguration, InputSettings, InputDevice, InputDeviceSettings, ScrollMethod, AccelerationProfile } from './api';
  import { copy, sameInput, currentDeviceSettings, capturedShortcut } from './input-preferences';

  let configuration = $state<InputConfiguration | null>(null);
  let saved = $state<InputSettings | null>(null);
  let expectedSaved = $state<InputSettings | null>(null);
  let draft = $state<InputSettings | null>(null);
  let devices = $state<InputDevice[]>([]);
  let { initialTab = 'keyboard' }: { initialTab?: 'keyboard' | 'pointer' } = $props();
  let tab = $state<'keyboard' | 'pointer' | 'shortcuts'>(untrack(() => initialTab));
  let selectedDevice = $state(''), capturing = $state<string | null>(null);
  let loading = $state(true), busy = $state(false), stale = $state(false);
  let error = $state(''), message = $state('');
  let polling = false, alive = true, revision = 0;
  let dirty = $derived(!!saved && !!draft && !sameInput(saved,draft));
  let externalSaved = $derived(!!saved && !!expectedSaved && !sameInput(saved,expectedSaved));
  let device = $derived(devices.find(device => device.id === selectedDevice));
  let preferences = $derived(device ? { ...currentDeviceSettings(device), ...draft?.devices[device.id] } : null);
  let customOptions = $derived(!!draft && !['','ctrl:nocaps','ctrl:swap_lalt_lctl'].includes(draft.keyboard_options));

  function capture(event: KeyboardEvent, action: string, fallback: string) {
    if (capturing !== action) return;
    event.preventDefault(); event.stopPropagation();
    if (event.key === 'Escape') { capturing = null; return; }
    const value = capturedShortcut(event);
    if (value) { shortcut(action, value, fallback); capturing = null; }
  }
  async function resetPointer() {
    try { const defaults = await invoke<Pick<InputSettings, 'pointer_speed' | 'natural_scroll' | 'workspace_swipe_fingers' | 'workspace_swipe_distance'>>('get_pointer_defaults'); if (draft) Object.assign(draft, defaults); }
    catch (cause) { error = String(cause); }
  }
  function accept(next: InputConfiguration, reset = false) {
    devices = next.devices;
    if (!devices.some(device => device.id === selectedDevice)) selectedDevice = devices[0]?.id ?? '';
    if (reset || !dirty) {
      configuration = next; saved = copy(next.input); draft = copy(next.input); expectedSaved = copy(next.saved_input); stale = false;
    } else if (saved && expectedSaved) stale = !sameInput(saved,next.input) || !sameInput(expectedSaved,next.saved_input);
  }
  async function refresh(reset = false) {
    if (polling || busy) return;
    const token = revision; polling = true;
    try { const next = await getInputConfiguration(); if (alive && token === revision) { accept(next,reset); if (reset) { error=''; message=''; } } }
    catch (cause) { if (alive && token === revision) error=String(cause); }
    finally { polling=false; if (alive) loading=false; }
  }
  async function apply() {
    if (!draft || !saved || !expectedSaved || busy) return;
    revision++; busy=true; error=''; message='';
    try { const next = await applyInputConfiguration(copy(saved),copy(expectedSaved),copy(draft)); if (alive) { accept(next,true); message='Keyboard and input settings are applied.'; } }
    catch (cause) { if (alive) error=String(cause); }
    finally { if (alive) busy=false; }
  }
  function discard() { if (saved) draft=copy(saved); error=''; message=''; }
  function setDevice<K extends keyof InputDeviceSettings>(key: K, value: InputDeviceSettings[K]) {
    if (!draft || !device) return;
    const settings = copy(draft.devices[device.id] ?? currentDeviceSettings(device));
    settings[key]=value;
    draft.devices[device.id]=settings;
  }
  function shortcut(action: string, value: string, defaultValue: string) {
    if (!draft) return;
    if (value === defaultValue) delete draft.keybindings[action]; else draft.keybindings[action]=value;
  }
  onMount(() => { void refresh(true); const timer=window.setInterval(() => void refresh(),2500); return () => { alive=false; window.clearInterval(timer); }; });
</script>

<div class="page-heading"><div><div class="eyebrow">A MORE NATURAL CONNECTION</div><h1>Keyboard & Input</h1><p>Make every keystroke, click, and gesture feel right.</p></div><button class="quiet" disabled={loading || busy} onclick={() => refresh(true)}>Reload</button></div>
{#if error}<div class="notice error" role="alert">{error}</div>{/if}
{#if message}<div class="notice success" role="status">{message}</div>{/if}
{#if stale}<div class="notice input-warning" role="status">Input preferences changed in another client. Reload before applying your draft.</div>{/if}
{#if externalSaved && !stale}<div class="notice input-warning" role="status"><span>Saved preferences differ from the active session. Load them to review and apply.</span><button disabled={busy} onclick={() => { if (expectedSaved) draft=copy(expectedSaved); }}>Load saved changes</button></div>{/if}
{#if loading}<div class="loading" role="status">Reading keyboard and input preferences…</div>
{:else if draft && configuration}
  <div class="input-tabs" role="group" aria-label="Input sections"><button aria-pressed={tab==='keyboard'} class:active={tab==='keyboard'} onclick={() => tab='keyboard'}>Keyboard</button><button aria-pressed={tab==='pointer'} class:active={tab==='pointer'} onclick={() => tab='pointer'}>Mouse & touchpad</button><button aria-pressed={tab==='shortcuts'} class:active={tab==='shortcuts'} onclick={() => tab='shortcuts'}>Shortcuts</button></div>
  {#if tab==='keyboard'}
    <section class="input-hero"><span class="input-hero-icon" aria-hidden="true">⌨</span><div><span class="eyebrow">YOUR WORDS, YOUR WAY</span><h2>{configuration.layouts.find(layout => layout.id===draft?.keyboard_layout)?.label ?? draft.keyboard_layout}</h2><p>Layout and repeat behavior are validated by the compositor.</p></div></section>
    <section class="section" aria-labelledby="typing-title"><h2 id="typing-title">Typing</h2><fieldset disabled={busy} class="input-fields"><legend class="sr-only">Keyboard preferences</legend><div class="settings-card">
      <div class="setting-row"><div><label for="input-layout">Keyboard layout</label><p>The layout used for typing across your desktop.</p></div><select id="input-layout" bind:value={draft.keyboard_layout} onchange={() => { if (draft) draft.keyboard_variant=''; }}>{#if !configuration.layouts.some(layout => layout.id===draft?.keyboard_layout)}<option value={draft.keyboard_layout}>{draft.keyboard_layout} (current)</option>{/if}{#each configuration.layouts as layout}<option value={layout.id}>{layout.label}</option>{/each}</select></div>
      <div class="setting-row"><div><label for="input-variant">Layout variant</label><p>Leave empty for the standard layout, or enter an XKB variant.</p></div><input id="input-variant" class="text-input" type="text" maxlength="128" bind:value={draft.keyboard_variant} placeholder="Default · e.g. dvorak" spellcheck="false" /></div>
      <div class="setting-row"><div><label for="input-modifiers">Modifier behavior</label><p>Choose a familiar arrangement for modifier keys.</p></div><select id="input-modifiers" bind:value={draft.keyboard_options}><option value="">Default</option><option value="ctrl:nocaps">Caps Lock as Ctrl</option><option value="ctrl:swap_lalt_lctl">Swap left Ctrl and Alt</option>{#if customOptions}<option value={draft.keyboard_options}>Current custom options</option>{/if}</select></div>
      <div class="setting-row"><div><label for="input-repeat-delay">Repeat delay</label><p>How long a key is held before repeating starts.</p></div><div class="input-range"><input id="input-repeat-delay" type="range" min="100" max="2000" step="25" bind:value={draft.keyboard_repeat_delay_ms} aria-valuetext={`${draft.keyboard_repeat_delay_ms} milliseconds`} /><output for="input-repeat-delay">{draft.keyboard_repeat_delay_ms} ms</output></div></div>
      <div class="setting-row"><div><label for="input-repeat-rate">Repeat rate</label><p>How quickly a held key repeats.</p></div><div class="input-range"><input id="input-repeat-rate" type="range" min="1" max="100" step="1" bind:value={draft.keyboard_repeat_rate} aria-valuetext={`${draft.keyboard_repeat_rate} keys per second`} /><output for="input-repeat-rate">{draft.keyboard_repeat_rate}/s</output></div></div>
    </div></fieldset></section>
    <section class="typing-test"><label for="input-try-typing">Try your current keyboard</label><p>Type here to check the active layout and repeat behavior. Draft changes take effect after Apply.</p><textarea id="input-try-typing" rows="3" placeholder="The quick brown fox…" spellcheck="false" aria-describedby="typing-private"></textarea><span id="typing-private">This text stays in this window and is never saved.</span></section>
  {:else if tab==='pointer'}
    <section class="section" aria-labelledby="pointer-title"><div class="section-heading"><div><h2 id="pointer-title">Pointer & scrolling</h2><p>Global preferences for your pointing devices.</p></div></div><fieldset disabled={busy} class="input-fields"><legend class="sr-only">Global pointer preferences</legend><div class="settings-card">
      <div class="setting-row"><div><label for="input-pointer-speed">Pointer speed</label><p>A desktop-wide multiplier for pointer movement.</p></div><div class="input-range"><input id="input-pointer-speed" type="range" min="0.25" max="2" step="0.05" bind:value={draft.pointer_speed} aria-valuetext={`${draft.pointer_speed.toFixed(2)} times`} /><output for="input-pointer-speed">{draft.pointer_speed.toFixed(2)}×</output></div></div>
      <div class="setting-row"><div><label for="input-natural-scroll">Natural scrolling</label><p>Move content in the same direction as your fingers.</p></div><input id="input-natural-scroll" class="switch" type="checkbox" role="switch" bind:checked={draft.natural_scroll} /></div>
    </div></fieldset></section>
    <section class="section" aria-labelledby="devices-title"><div class="section-heading"><div><h2 id="devices-title">Your devices</h2><p>Available controls follow each device's reported capabilities.</p></div><span class="count">{devices.length} devices</span></div>
      {#if !devices.length}<div class="input-empty"><span aria-hidden="true">◈</span><h3>{configuration.hardware_devices ? 'No pointer devices reported' : 'Input devices belong to your host desktop'}</h3><p>{configuration.hardware_devices ? 'Connect a mouse or touchpad, then reload this page.' : 'Use a FocalDesk DRM session for direct mouse and touchpad controls. Keyboard and global pointer preferences can still be tested here.'}</p></div>
      {:else if device && preferences}<div class="settings-card device-card"><div class="setting-row"><div><label for="input-device">Mouse or touchpad</label><p>{device.touchpad ? 'Touchpad' : 'Pointer device'} · {device.enabled ? 'Enabled' : 'Disabled'} · {device.sysname}</p></div><select id="input-device" bind:value={selectedDevice} disabled={busy}>{#each devices as candidate}<option value={candidate.id}>{candidate.name}</option>{/each}</select></div>
      <fieldset disabled={busy} class="input-fields"><legend class="sr-only">Per-device preferences</legend>
        <div class="setting-row"><div><label for="input-device-enabled">Device enabled</label><p>{device.can_disable ? 'Changes take effect after Apply.' : 'Keyboard-capable or unsupported devices must stay enabled.'}</p></div><input id="input-device-enabled" class="switch" type="checkbox" role="switch" disabled={!device.can_disable} checked={preferences.enabled ?? device.enabled} onchange={event => setDevice('enabled',event.currentTarget.checked)} /></div>
        {#if device.supports_acceleration}<div class="setting-row"><div><label for="input-device-speed">Device acceleration speed</label><p>Driver-level sensitivity for this device.</p></div><div class="input-range"><input id="input-device-speed" type="range" min="-1" max="1" step="0.05" value={preferences.acceleration_speed ?? device.acceleration_speed} oninput={event => setDevice('acceleration_speed',Number(event.currentTarget.value))} /><output for="input-device-speed">{(preferences.acceleration_speed ?? device.acceleration_speed).toFixed(2)}</output></div></div>
        <div class="setting-row"><div><label for="input-device-acceleration">Acceleration profile</label><p>Adaptive follows movement speed; flat keeps a constant response.</p></div><select id="input-device-acceleration" value={preferences.acceleration_profile} onchange={event => setDevice('acceleration_profile',event.currentTarget.value as AccelerationProfile)}><option value="default">Device default</option>{#if device.supports_adaptive_acceleration}<option value="adaptive">Adaptive</option>{/if}{#if device.supports_flat_acceleration}<option value="flat">Flat</option>{/if}</select></div>{/if}
        {#if device.tap_finger_count>0}<div class="setting-row"><div><label for="input-device-tap">Tap to click</label><p>A light touch becomes a click.</p></div><input id="input-device-tap" class="switch" type="checkbox" role="switch" checked={preferences.tap_to_click ?? device.tap_to_click} onchange={event => setDevice('tap_to_click',event.currentTarget.checked)} /></div>{/if}
        {#if device.supports_disable_while_typing}<div class="setting-row"><div><label for="input-device-typing">Disable touchpad while typing</label><p>Keep accidental touches out of your way.</p></div><input id="input-device-typing" class="switch" type="checkbox" role="switch" checked={preferences.disable_while_typing ?? device.disable_while_typing} onchange={event => setDevice('disable_while_typing',event.currentTarget.checked)} /></div>{/if}
        {#if device.supports_left_handed}<div class="setting-row"><div><label for="input-device-left">Left-handed buttons</label><p>Swap primary and secondary mouse buttons.</p></div><input id="input-device-left" class="switch" type="checkbox" role="switch" checked={preferences.left_handed ?? device.left_handed} onchange={event => setDevice('left_handed',event.currentTarget.checked)} /></div>{/if}
        {#if device.supports_middle_button_emulation}<div class="setting-row"><div><label for="input-device-middle">Emulate middle button</label><p>Press the primary and secondary buttons together.</p></div><input id="input-device-middle" class="switch" type="checkbox" role="switch" checked={preferences.middle_button_emulation ?? device.middle_button_emulation} onchange={event => setDevice('middle_button_emulation',event.currentTarget.checked)} /></div>{/if}
        {#if device.supports_two_finger_scroll || device.supports_edge_scroll || device.supports_button_scroll || device.supports_disabled_scroll}<div class="setting-row"><div><label for="input-device-scroll">Scrolling method</label><p>Choose a method supported by this device.</p></div><select id="input-device-scroll" value={preferences.scroll_method} onchange={event => setDevice('scroll_method',event.currentTarget.value as ScrollMethod)}><option value="default">Device default</option>{#if device.supports_two_finger_scroll}<option value="two_finger">Two fingers</option>{/if}{#if device.supports_edge_scroll}<option value="edge">Edge scrolling</option>{/if}{#if device.supports_button_scroll}<option value="on_button_down">While holding a button</option>{/if}{#if device.supports_disabled_scroll}<option value="disabled">Disabled</option>{/if}</select></div>{/if}
      </fieldset><div class="device-live"><span class="connection-dot"></span>Live: {device.tap_finger_count>0 ? `tap to click ${device.tap_to_click ? 'on' : 'off'} · ` : ''}{device.acceleration_profile ?? 'device default'} acceleration{device.supports_acceleration ? ` · ${device.acceleration_speed.toFixed(2)}` : ''}</div></div>{/if}
    </section>
    <section class="section" aria-labelledby="gestures-title"><h2 id="gestures-title">Workspace gestures</h2><fieldset disabled={busy} class="input-fields"><legend class="sr-only">Workspace gesture preferences</legend><div class="settings-card"><div class="setting-row"><div><label for="input-swipe-fingers">Workspace swipe</label><p>Reserve horizontal swipes for changing workspaces.</p></div><select id="input-swipe-fingers" bind:value={draft.workspace_swipe_fingers}><option value={0}>Disabled</option><option value={3}>Three fingers</option><option value={4}>Four fingers</option></select></div><div class="setting-row"><div><label for="input-swipe-distance">Activation distance</label><p>How far to swipe before changing workspace.</p></div><div class="input-range"><input id="input-swipe-distance" type="range" min="40" max="400" step="10" bind:value={draft.workspace_swipe_distance} /><output for="input-swipe-distance">{draft.workspace_swipe_distance} px</output></div></div></div></fieldset></section>
    <section class="section"><div class="setting-row"><div><strong>Reset pointer and gestures</strong><p>Restore defaults in this draft. Apply to activate them.</p></div><button class="quiet" disabled={busy} onclick={resetPointer}>Reset pointer and gestures</button></div></section>
  {:else}
    <section class="section" aria-labelledby="shortcuts-title"><div class="section-heading"><div><h2 id="shortcuts-title">Your shortcuts</h2><p>Enter combinations such as Ctrl+Alt+T. Rust checks conflicts before applying.</p></div><button class="quiet" disabled={busy} onclick={() => { if (draft) draft.keybindings={}; }}>Restore defaults</button></div><div class="settings-card"><fieldset disabled={busy} class="input-fields"><legend class="sr-only">Shortcut overrides</legend>
      {#each configuration.shortcuts.filter(shortcut => shortcut.default || draft?.keybindings[shortcut.action] !== undefined) as binding}<div class="setting-row shortcut-row"><div><label for={`shortcut-${binding.action}`}>{binding.label}</label><p>{binding.default ? `Default: ${binding.default}` : 'Additional shortcut override'}</p></div><div class="shortcut-control"><input id={`shortcut-${binding.action}`} class="text-input" type="text" maxlength="128" spellcheck="false" autocomplete="off" placeholder={capturing === binding.action ? 'Press keys; Escape cancels' : ''} onkeydown={event => capture(event,binding.action,binding.default)} onblur={() => capturing = null} value={capturing === binding.action ? '' : draft.keybindings[binding.action] ?? binding.default} oninput={event => shortcut(binding.action,event.currentTarget.value,binding.default)} /><button class="quiet" aria-label={`Capture shortcut for ${binding.label}`} onclick={() => { capturing = binding.action; document.getElementById(`shortcut-${binding.action}`)?.focus(); }}>Capture</button><button class="quiet" aria-label={`Restore default for ${binding.label}`} disabled={draft.keybindings[binding.action] === undefined} onclick={() => { if (draft) delete draft.keybindings[binding.action]; }}>Reset</button></div></div>{/each}
    </fieldset></div><p class="footnote">Enter shortcut names or choose Capture and press a combination. Escape cancels capture. Desktop-reserved shortcuts may be intercepted; use text entry for those. Changes take effect after Apply.</p></section>
  {/if}
{/if}
<footer class="apply-bar"><div><span class="status-dot" class:unsaved={dirty}></span>{stale ? 'Reload to resolve changed settings' : dirty ? 'You have unapplied input changes' : externalSaved ? 'Saved changes are not active yet' : 'Your input preferences are up to date'}</div><div class="apply-actions"><button class="quiet" disabled={!dirty || busy} onclick={discard}>Discard</button><button class="primary" disabled={!dirty || stale || busy || loading} onclick={apply}>{busy ? 'Applying…' : 'Apply changes'}</button></div></footer>

<style>
  .input-tabs{display:flex;gap:5px;padding:5px;border:1px solid var(--fd-border);border-radius:11px;background:#eeeff6;margin-bottom:25px;width:fit-content}.input-tabs button{padding:10px 17px;border-radius:7px;background:transparent;font-size:11px;color:#767d93}.input-tabs button.active{color:var(--fd-accent);background:var(--fd-surface);box-shadow:0 2px 6px #20263b08}.input-hero{padding:25px;border:1px solid #e1def1;border-radius:var(--fd-radius);background:linear-gradient(125deg,#eeebfc,#fdfcff);display:flex;gap:18px;align-items:center}.input-hero-icon{width:54px;height:54px;display:grid;place-items:center;font-size:27px;color:var(--fd-accent);background:white;border:1px solid #e4e0f6;border-radius:16px}.input-hero .eyebrow{font-size:8px;display:block;margin-bottom:8px}.input-hero h2{font-size:19px}.input-hero p{font-size:11px;color:var(--fd-text-muted);margin-top:9px}.input-fields{padding:0;margin:0;border:0;min-width:0}.input-fields select,.text-input{padding:10px 12px;font-size:11px;border:1px solid var(--fd-border);border-radius:8px;background:var(--fd-surface);color:var(--fd-text);max-width:240px;min-width:165px}.device-card> .setting-row select{width:45%;max-width:270px;padding:10px 12px;border:1px solid var(--fd-border);border-radius:8px;font-size:11px;color:var(--fd-text);background:var(--fd-surface)}.input-range{display:flex;align-items:center;gap:15px}.input-range input{width:135px;accent-color:var(--fd-accent)}.input-range output{min-width:58px;text-align:right;font-size:11px;color:var(--fd-text-muted)}.typing-test{padding:22px;border:1px solid var(--fd-border);border-radius:var(--fd-radius);background:var(--fd-surface);margin-top:24px}.typing-test>label{font-size:13px;font-weight:600}.typing-test p{font-size:10px;line-height:1.7;color:var(--fd-text-muted);margin-top:7px}.typing-test textarea{display:block;width:100%;resize:vertical;min-height:90px;background:#f9f9fd;border:1px solid var(--fd-border);border-radius:9px;padding:14px;margin-top:15px;font:inherit;color:var(--fd-text)}.typing-test>span{display:block;font-size:9px;color:#969bad;margin-top:10px}.device-live{border-top:1px solid var(--fd-border);padding:13px 20px;color:#8992a7;font-size:10px}.connection-dot{display:inline-block;width:5px;height:5px;border-radius:50%;background:#65a48e;margin-right:6px}.input-empty{text-align:center;border:1px dashed #d9dbe9;border-radius:var(--fd-radius);padding:30px;color:var(--fd-text-muted)}.input-empty>span{font-size:28px;color:#aaa1d5}.input-empty h3{font-size:13px;font-weight:550;margin:12px 0 8px}.input-empty p{font-size:11px;line-height:1.8;max-width:420px;margin:auto}.shortcut-control{display:flex;gap:8px;align-items:center}.shortcut-control .text-input{width:180px;min-width:100px;font-family:ui-monospace,monospace}.shortcut-row .quiet{font-size:9px;padding:9px 10px}.input-warning{background:#fff8e9;color:#8a652b;border:1px solid #ebd7b2}.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}@media(max-width:950px){.input-range{gap:8px}.input-range input{width:90px}.input-range output{min-width:50px}.input-fields select,.text-input{min-width:135px;max-width:200px}.shortcut-control .text-input{width:140px}.shortcut-control{gap:5px}.input-hero{padding:20px}.input-hero p{line-height:1.7}.input-tabs button{padding:9px 12px}.shortcut-row{gap:12px}}@media(max-width:600px){.input-fields select,.text-input{width:100%;max-width:none}.input-range{width:100%}.input-range input{flex:1}.shortcut-control{width:100%}.shortcut-control .text-input{flex:1}.input-tabs button{padding:8px 10px;font-size:10px}}
</style>
