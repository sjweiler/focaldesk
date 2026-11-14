<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  let { connector, disabled }: { connector: string; disabled: boolean } = $props();
  type State = { phase: string; connector: string | null; reason: string | null; session_id: number | null };
  let hdrState = $state<State | null>(null), confirmation = $state(false), busy = $state(false), error = $state(''), message = $state('');
  let active = $derived(hdrState?.connector === connector && hdrState?.phase === 'active');
  async function reload() { try { hdrState = await invoke<State>('get_exclusive_hdr'); } catch (cause) { error = String(cause); } }
  async function restart() {
    if (!hdrState || busy) return; busy = true; confirmation = false; error = '';
    try { message = await invoke<string>('restart_exclusive_hdr', { connector, expected: hdrState, enabled: !active }); await reload(); }
    catch (cause) { error = String(cause); } finally { busy = false; }
  }
  onMount(() => { void reload(); });
</script>
<section class="section"><h2>Experimental exclusive HDR10</h2><div class="settings-card"><div class="setting-row"><div><strong>HDR session mode</strong><p>Restarts the desktop session in guarded exclusive HDR mode. Save your work first; other displays may be unavailable.</p><p>Status: {hdrState?.phase ?? 'Loading'}{hdrState?.connector ? ` · ${hdrState.connector}` : ''}</p>{#if hdrState?.reason}<p>{hdrState.reason}</p>{/if}</div><button class="quiet" disabled={disabled || busy || !hdrState || ['starting','verifying'].includes(hdrState.phase)} onclick={() => confirmation = true}>{active ? 'Disable and restart…' : 'Restart and try HDR10…'}</button></div></div>
{#if confirmation}<div class="notice" role="alertdialog" aria-label="Restart HDR session"><p>Save your work. This logs out your desktop session. {active ? 'Return to normal multi-monitor mode?' : `Try exclusive HDR10 on ${connector}?`}</p><button class="quiet" onclick={() => confirmation = false}>Cancel</button><button class="primary" onclick={restart}>Save HDR mode and log out</button></div>{/if}
{#if error}<div class="notice error" role="alert">{error}</div>{/if}{#if message}<div class="notice" role="status">{message}</div>{/if}</section>
