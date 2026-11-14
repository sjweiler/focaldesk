<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import PreferenceOperations from './PreferenceOperations.svelte';
  import { copy } from './input-preferences';
  type Privacy = { recent_files: boolean; location_services: boolean; hide_lock_screen_notifications: boolean; notification_history_limit: number; clear_notification_history_on_logout: boolean };
  type Power = { blank_screen_minutes: number | null; suspend_minutes: number | null; power_button_action: string; lid_close_action: string; low_battery_action: string; performance_mode: string };
  type Preferences = { privacy: Privacy; power: Power };
  let { section }: { section: 'privacy' | 'power' } = $props();
  let saved = $state<Preferences | null>(null), draft = $state<Preferences | null>(null);
  let busy = $state(false), error = $state(''), message = $state('');
  let dirty = $derived(!!saved && !!draft && JSON.stringify(saved[section]) !== JSON.stringify(draft[section]));
  const privacySwitches = [
    ['location_services', 'Location services', 'Allow location-aware apps to request location access.'],
    ['recent_files', 'Recent activity', 'Remember launched applications and opened files for Launcher search.'],
    ['hide_lock_screen_notifications', 'Hide lock screen notifications', 'Keep notification contents private while the screen is locked.'],
    ['clear_notification_history_on_logout', 'Clear notification history on logout', 'Start each session with an empty notification history.'],
  ] as const;
  const actions = [
    { key: 'power_button_action', label: 'Power button', options: [['show_power_menu','Show power menu'],['suspend','Suspend'],['power_off','Power off'],['do_nothing','Do nothing']] },
    { key: 'lid_close_action', label: 'When the lid closes', options: [['suspend','Suspend'],['blank_screen','Blank screen'],['lock_screen','Lock screen'],['do_nothing','Do nothing']] },
    { key: 'low_battery_action', label: 'Low battery', options: [['notify_only','Notify only'],['suspend','Suspend'],['hibernate','Hibernate'],['power_off','Power off']] },
    { key: 'performance_mode', label: 'Performance profile', options: [['balanced','Balanced'],['performance','Performance'],['power_saver','Power saver']] },
  ] as const;
  async function reload() {
    busy=true; error=''; message='';
    try { saved=await invoke<Preferences>('get_preferences'); draft=copy(saved); }
    catch (cause) { error=String(cause); } finally { busy=false; }
  }
  async function apply() {
    if (!saved || !draft) return;
    busy=true; error=''; message='';
    try {
      const result=await invoke<{ preferences: Preferences; message: string }>('apply_preferences', { change: { section, expected: saved[section], requested: draft[section] } });
      saved=result.preferences; draft=copy(saved); message=result.message;
    } catch (cause) { error=String(cause); } finally { busy=false; }
  }
  onMount(() => { void reload(); });
</script>

<div class="page-heading"><div><div class="eyebrow">{section==='privacy' ? 'YOUR DESKTOP, YOUR CHOICE' : 'A LITTLE MORE BALANCE'}</div><h1>{section==='privacy' ? 'Privacy' : 'Power'}</h1><p>{section==='privacy' ? 'Choose what your desktop remembers and shares.' : 'Set idle behavior and choose how your desktop uses energy.'}</p></div><button class="quiet" disabled={busy} onclick={reload}>Reload</button></div>
{#if error}<div class="notice error" role="alert">{error}</div>{/if}
{#if message}<div class="notice success" role="status">{message}{#if section === 'privacy' && message.includes('retention')}<button class="quiet" disabled={busy} onclick={apply}>Retry live update</button>{/if}</div>{/if}
{#if draft}
<fieldset disabled={busy}>
  {#if section==='privacy'}
    <section class="section"><h2>Privacy preferences</h2><div class="settings-card">
      {#each privacySwitches as [key,label,description]}<div class="setting-row"><div><label for={`privacy-${key}`}>{label}</label><p>{description}</p></div><input id={`privacy-${key}`} type="checkbox" role="switch" class="switch" bind:checked={draft.privacy[key]} /></div>{/each}
      <div class="setting-row"><div><label for="privacy-history">Notification history retention</label><p>Maximum saved notification entries.</p></div><select id="privacy-history" bind:value={draft.privacy.notification_history_limit}>{#if ![25,50,100].includes(draft.privacy.notification_history_limit)}<option value={draft.privacy.notification_history_limit}>{draft.privacy.notification_history_limit} (current)</option>{/if}{#each [25,50,100] as limit}<option value={limit}>{limit} entries</option>{/each}</select></div>
    </div></section>
    <section class="section"><h2>App permissions</h2><div class="settings-card"><div class="setting-row"><div><strong>Screen sharing</strong><p>The portal asks you to choose a screen for every request.</p></div><span>Ask each time</span></div><div class="setting-row"><div><strong>Microphone and camera portals</strong><p>Per-app portal controls are unavailable in this build.</p></div></div></div></section>
  {:else}
    <section class="section"><h2>Idle</h2><div class="settings-card">
      <div class="setting-row"><div><label for="power-blank">Blank screen after</label><p>Turn off display output after inactivity.</p></div><select id="power-blank" bind:value={draft.power.blank_screen_minutes}><option value={null}>Never</option>{#if draft.power.blank_screen_minutes !== null && ![1,5,10,30].includes(draft.power.blank_screen_minutes)}<option value={draft.power.blank_screen_minutes}>{draft.power.blank_screen_minutes} minutes (current)</option>{/if}{#each [1,5,10,30] as minutes}<option value={minutes}>{minutes} minutes</option>{/each}</select></div>
      <div class="setting-row"><div><label for="power-suspend">Suspend after</label><p>Suspend the session after longer inactivity.</p></div><select id="power-suspend" bind:value={draft.power.suspend_minutes}><option value={null}>Never</option>{#if draft.power.suspend_minutes !== null && ![15,30,60].includes(draft.power.suspend_minutes)}<option value={draft.power.suspend_minutes}>{draft.power.suspend_minutes} minutes (current)</option>{/if}{#each [15,30,60] as minutes}<option value={minutes}>{minutes} minutes</option>{/each}</select></div>
    </div></section>
    <section class="section"><h2>Behavior</h2><div class="settings-card">{#each actions as action}<div class="setting-row"><div><label for={`power-${action.key}`}>{action.label}</label>{#if action.key==='performance_mode'}<p>Uses the FocalDesk power service and power-profiles-daemon.</p>{/if}</div><select id={`power-${action.key}`} bind:value={draft.power[action.key]}>{#each action.options as [value,label]}<option {value}>{label}</option>{/each}</select></div>{/each}</div></section>
    
  {/if}
</fieldset>
<PreferenceOperations {section} />
{:else if busy}<div class="loading" role="status">Reading preferences…</div>{/if}
<footer class="apply-bar"><div><span class="status-dot" class:unsaved={dirty}></span>{dirty ? 'You have unapplied changes' : 'Preferences match the saved configuration'}</div><div class="apply-actions"><button class="quiet" disabled={!dirty || busy} onclick={() => { if (saved) draft=copy(saved); error=''; message=''; }}>Discard</button><button class="primary" disabled={!dirty || busy} onclick={apply}>{busy ? 'Applying…' : 'Apply changes'}</button></div></footer>

<style>
  fieldset{border:0;padding:0;margin:0;min-width:0}select{padding:10px 12px;border:1px solid var(--fd-border);border-radius:8px;background:var(--fd-surface);color:var(--fd-text);font:inherit;font-size:12px;max-width:240px}.setting-row>span{color:var(--fd-text-muted);font-size:12px}.setting-row strong{font-size:13px;font-weight:550}
</style>
