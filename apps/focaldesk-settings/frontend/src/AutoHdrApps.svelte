<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';

  let { disabled = false }: { disabled?: boolean } = $props();
  let saved = $state<string | null>(null);
  let draft = $state('');
  let busy = $state(false);
  let error = $state('');
  let message = $state('');

  async function load() {
    busy = true;
    error = '';
    try {
      const titles = await invoke<string[]>('get_auto_hdr_xwayland_titles');
      saved = draft = titles.join('\n');
    } catch (cause) {
      error = String(cause);
    } finally {
      busy = false;
    }
  }

  async function apply() {
    if (busy || disabled || saved === null || draft === saved) return;
    busy = true;
    error = '';
    message = '';
    try {
      const titles = draft.split('\n').map(title => title.trim()).filter(Boolean);
      await invoke('set_auto_hdr_xwayland_titles', { titles });
      saved = draft = titles.join('\n');
      message = 'Auto HDR app list applied.';
    } catch (cause) {
      error = String(cause);
    } finally {
      busy = false;
    }
  }

  onMount(() => { void load(); });
</script>

<section class="section" aria-labelledby="auto-hdr-title">
  <h2 id="auto-hdr-title">Auto HDR</h2>
  {#if error}<div class="notice error" role="alert">{error}</div>{/if}
  {#if message}<div class="notice success" role="status">{message}</div>{/if}
  {#if saved !== null}
    <label for="auto-hdr-apps">XWayland window titles</label>
    <p id="auto-hdr-help">Enter one exact title per line. Auto HDR enhances SDR windows on active HDR displays. Clear the list to turn it off.</p>
    <textarea id="auto-hdr-apps" rows="3" placeholder="World of Warcraft" aria-describedby="auto-hdr-help" bind:value={draft} disabled={busy || disabled}></textarea>
    <button class="quiet" onclick={apply} disabled={busy || disabled || draft === saved}>Apply Auto HDR list</button>
  {:else}
    <button class="quiet" onclick={load} disabled={busy || disabled}>{busy ? 'Reading Auto HDR list…' : 'Reload Auto HDR list'}</button>
  {/if}
</section>

<style>
  label { display: block; margin-top: 12px; }
  p { margin: 6px 0 12px; color: var(--fd-text-muted); }
  textarea {
    display: block;
    width: 100%;
    padding: 10px;
    margin-bottom: 12px;
    border: 1px solid var(--fd-border);
    border-radius: 8px;
    background: var(--fd-surface);
    color: var(--fd-text);
    font: inherit;
    resize: vertical;
  }
</style>
