<script lang="ts">
  import {onMount} from 'svelte';
  import {invoke} from '@tauri-apps/api/core';
  interface Preferences {topbar_on_all_outputs:boolean;sidebar_on_all_outputs:boolean;remember_focused_output:boolean}
  let {disabled=false}:{disabled?:boolean}=$props();
  let saved=$state<Preferences|null>(null),draft=$state<Preferences|null>(null),busy=$state(false),error=$state(''),message=$state('');
  let alive=true;
  const controls=[['topbar_on_all_outputs','Show top bar on all displays','When off, only the focused display shows the top bar.'],['sidebar_on_all_outputs','Show sidebar on all displays','When off, only the focused display shows the sidebar.'],['remember_focused_output','Remember focused display','Restore the last active display when FocalDesk starts.']] as const;
  let dirty=$derived(!!saved&&!!draft&&JSON.stringify(saved)!==JSON.stringify(draft));
  async function reload(){busy=true;error='';message='';try{const next=await invoke<Preferences>('get_display_layout_preferences');if(alive){saved={...next};draft={...next};}}catch(cause){if(alive)error=String(cause);}finally{if(alive)busy=false;}}
  async function apply(){if(!saved||!draft||busy||disabled)return;busy=true;error='';message='';try{const next=await invoke<Preferences>('apply_display_layout_preferences',{expected:{...saved},requested:{...draft}});if(alive){saved={...next};draft={...next};message='Display layout preferences applied. Focus restoration takes effect at the next sign-in.';}}catch(cause){if(alive)error=String(cause);}finally{if(alive)busy=false;}}
  onMount(()=>{void reload();return()=>{alive=false;};});
</script>
<section class="section"><div class="section-heading"><div><h2>Desktop layout & focus</h2><p>These preferences are saved separately from monitor modes and arrangement.</p></div><button class="quiet" disabled={busy||disabled} onclick={reload}>Reload layout preferences</button></div>
{#if error}<div class="notice error" role="alert">{error}</div>{/if}{#if message}<div class="notice success" role="status">{message}</div>{/if}
{#if draft}<fieldset disabled={busy||disabled}><legend class="sr-only">Desktop layout preferences</legend><div class="settings-card">{#each controls as [key,label,description]}<div class="setting-row"><div><label for={`layout-${key}`}>{label}</label><p>{description}</p></div><input id={`layout-${key}`} class="switch" type="checkbox" role="switch" bind:checked={draft[key]} /></div>{/each}</div><div class="layout-actions"><button class="quiet" disabled={!dirty} onclick={()=>{if(saved)draft={...saved};error='';message='';}}>Discard layout preferences</button><button class="quiet" disabled={!dirty} onclick={apply}>Apply layout preferences</button></div></fieldset>{:else if busy}<p role="status">Reading layout preferences…</p>{/if}</section>
<style>fieldset{padding:0;border:0;margin:0;min-width:0}.layout-actions{display:flex;gap:10px;justify-content:flex-end;margin-top:18px}.section-heading p{font-size:12px;color:var(--fd-text-muted);line-height:1.8;margin-top:10px}.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}</style>
