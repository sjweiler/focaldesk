<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  let { section }: { section: 'privacy' | 'power' } = $props();
  type Permission = { id: string; app: string; description: string };
  type Power = { batteries: { name: string; percentage: number | null; state: string | null }[]; line_power_online: boolean | null; performance_profile: string | null };
  let permissions = $state<Permission[]>([]), errors = $state<string[]>([]);
  let power = $state<Power | null>(null), busy = $state(false), error = $state(''), message = $state('');
  let confirmation = $state<{ label: string; command: string; args: Record<string, string> } | null>(null);
  async function reload() {
    busy = true; error = '';
    try {
      if (section === 'privacy') { const result = await invoke<{ records: Permission[]; errors: string[] }>('get_saved_permissions'); permissions = result.records; errors = result.errors; }
      else power = await invoke<Power>('get_power_status');
    } catch (cause) { if (section === 'power') power = null; error = String(cause); } finally { busy = false; }
  }
  async function execute() {
    const operation = confirmation; if (!operation || busy) return;
    confirmation = null; busy = true; error = ''; message = '';
    try { const result = await invoke<string | null>(operation.command, operation.args); message = result || `${operation.label} completed.`; await reload(); }
    catch (cause) { error = String(cause); } finally { busy = false; }
  }
  onMount(() => { void reload(); });
</script>
<section class="section">
  <div class="section-heading"><h2>{section === 'privacy' ? 'Saved permissions and activity' : 'Live power status and actions'}</h2><button class="quiet" disabled={busy} onclick={reload}>Refresh status</button></div>
  {#if error}<div class="notice error" role="alert">{error}</div>{/if}
  {#each errors as warning}<div class="notice error" role="alert">{warning}</div>{/each}
  {#if message}<div class="notice" role="status">{message}</div>{/if}
  <div class="settings-card">
    {#if section === 'privacy'}
      {#each permissions as record (record.id)}<div class="setting-row"><div><strong>{record.app}</strong><p>{record.description}</p></div><button class="quiet" disabled={busy} onclick={() => confirmation = { label: `Revoke permission for ${record.app}`, command: 'revoke_saved_permission', args: { id: record.id } }}>Revoke…</button></div>{/each}
      {#if !permissions.length && !busy}<div class="setting-row">No saved permissions were returned.</div>{/if}
      <div class="setting-row"><div><strong>Clear recent activity</strong><p>Remove applications and files from Launcher’s recent results.</p></div><button class="quiet" disabled={busy} onclick={() => confirmation = { label: 'Clear recent activity', command: 'clear_recent_activity', args: {} }}>Clear…</button></div>
    {:else}
      {#if power}<div class="setting-row"><span>Power source</span><strong>{!power.batteries.length || power.line_power_online === true ? 'External power' : power.line_power_online === false ? 'Battery' : 'Unknown'}</strong></div><div class="setting-row"><span>Active performance profile</span><strong>{power.performance_profile ?? 'Unavailable'}</strong></div>{#each power.batteries as battery}<div class="setting-row"><span>{battery.name}</span><strong>{battery.percentage === null ? 'Unknown charge' : `${battery.percentage}%`} · {battery.state ?? 'Unknown state'}</strong></div>{/each}{#if !power.batteries.length}<div class="setting-row">No battery reported.</div>{/if}{/if}
      {#each [['suspend','Suspend'],['hibernate','Hibernate'],['reboot','Restart'],['power_off','Shut down']] as [action,label]}<div class="setting-row"><strong>{label}</strong><button class="quiet" disabled={busy || !power} onclick={() => confirmation = { label, command: 'perform_power_action', args: { action } }}>{label}…</button></div>{/each}
    {/if}
  </div>
  {#if confirmation}<div class="notice" role="alertdialog" aria-label={confirmation.label}><p>{confirmation.label}? {section === 'power' ? 'Save your work before continuing. This can end your session.' : 'This removes the saved record and cannot be undone.'}</p><button class="quiet" onclick={() => confirmation = null}>Cancel</button><button class="primary" disabled={busy} onclick={execute}>Confirm {confirmation.label}</button></div>{/if}
</section>
