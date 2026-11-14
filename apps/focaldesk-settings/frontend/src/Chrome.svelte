<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { copy } from './input-preferences';
  interface Region {order:number[];hidden:number[];custom:{id:number;icon:string;tooltip:string;command:string;enabled:boolean}[]}
  interface Chrome {topbar:Region;sidebar:Region}
  interface Snapshot {chrome:Chrome;path:string}
  const regions = [
    {key:'topbar' as const,title:'Top-bar indicators',description:'Choose the live indicators shown in your desktop top bar.',items:[[100,'Network'],[101,'Bluetooth'],[102,'Audio'],[103,'HDR display status'],[104,'Power'],[105,'Camera'],[106,'Do Not Disturb'],[107,'Notifications'],[108,'Microphone'],[109,'System updates']] as const},
    {key:'sidebar' as const,title:'Sidebar buttons',description:'Choose fixed application buttons. Workspace controls remain dynamic.',items:[[1001,'Settings'],[1000,'Launcher'],[1005,'Browser'],[1006,'Terminal'],[1007,'Files']] as const},
  ];
  let saved=$state<Chrome|null>(null),draft=$state<Chrome|null>(null),orders=$state({topbar:'',sidebar:''}),path=$state(''),busy=$state(false),loading=$state(true),error=$state(''),message=$state(''),resetPending=$state(false);
  let alive=true;
  let dirty=$derived(!!saved&&!!draft&&(JSON.stringify(draft)!==JSON.stringify(saved)||orders.topbar!==saved.topbar.order.join(', ')||orders.sidebar!==saved.sidebar.order.join(', ')));
  function accept(next:Snapshot){saved=copy(next.chrome);draft=copy(next.chrome);orders={topbar:next.chrome.topbar.order.join(', '),sidebar:next.chrome.sidebar.order.join(', ')};path=next.path;resetPending=false;}
  async function reload(){busy=true;error='';message='';try{const next=await invoke<Snapshot>('get_chrome');if(alive)accept(next);}catch(cause){if(alive)error=String(cause);}finally{if(alive){busy=false;loading=false;}}}
  function discard(){if(saved)accept({chrome:copy(saved),path});error='';message='';}
  function toggle(key:'topbar'|'sidebar',id:number,visible:boolean){if(!draft)return;draft[key].hidden=draft[key].hidden.filter(item=>item!==id);if(!visible)draft[key].hidden.push(id);}
  function reset(){if(!draft)return;for(const region of regions){const ids=region.items.map(([id])=>id) as readonly number[];draft[region.key].hidden=draft[region.key].hidden.filter(id=>!ids.includes(id));orders[region.key]='';}resetPending=false;}
  async function apply(){if(!saved||!draft||busy)return;busy=true;error='';message='';try{const next=await invoke<{snapshot:Snapshot;message:string}>('apply_chrome',{change:{expected:copy(saved),topbar:{order:orders.topbar,hidden:[...draft.topbar.hidden]},sidebar:{order:orders.sidebar,hidden:[...draft.sidebar.hidden]}}});if(alive){accept(next.snapshot);message=next.message;}}catch(cause){if(alive)error=String(cause);}finally{if(alive)busy=false;}}
  onMount(()=>{void reload();return()=>{alive=false;};});
</script>

<div class="page-heading"><div><div class="eyebrow">YOUR DESKTOP CONTROLS</div><h1>Chrome</h1><p>Arrange desktop indicators and sidebar buttons.</p></div><button class="quiet" disabled={busy} onclick={reload}>Reload chrome</button></div>
{#if error}<div class="notice error" role="alert">{error}</div>{/if}
{#if message}<div class="notice success" role="status">{message}</div>{/if}
{#if loading}<div class="loading" role="status">Reading desktop chrome preferences…</div>
{:else if draft}
<fieldset disabled={busy}><legend class="sr-only">Desktop chrome controls</legend>
{#each regions as region}
<section class="section"><h2>{region.title}</h2><p class="description">{region.description}</p><div class="settings-card">
{#each region.items as [id,label]}<div class="setting-row"><div><label for={`chrome-${id}`}>{label}</label><p>Item ID {id}</p></div><input id={`chrome-${id}`} class="switch" type="checkbox" role="switch" checked={!draft[region.key].hidden.includes(id)} onchange={event=>toggle(region.key,id,event.currentTarget.checked)} /></div>{/each}
<div class="setting-row"><div><label for={`chrome-order-${region.key}`}>{region.key==='topbar'?'Indicator order':'Fixed-item priority'}</label><p>Comma-separated item IDs. Unlisted items follow in their default order. Leave blank for the default order.</p></div><input id={`chrome-order-${region.key}`} class="order-input" type="text" maxlength="65536" bind:value={orders[region.key]} autocomplete="off" /></div>
</div></section>
{/each}
<section class="section"><h2>Custom launch items</h2><p class="description">Custom launch items are preserved. Edit their definitions in your configuration file.</p><div class="settings-card"><div class="setting-row"><div><strong>Configuration file</strong><p class="configuration-path">{path}</p></div></div>{#each regions as region}{#each draft[region.key].custom as item}<div class="setting-row"><div><strong>{item.tooltip||`Item ${item.id}`}</strong><p>{region.title} · ID {item.id} · {item.enabled?'Enabled':'Disabled'}</p></div></div>{/each}{/each}</div></section>
<section class="section"><h2>Restore built-in controls</h2><p class="description">Show every built-in item and reset ordering. Custom definitions and visibility are preserved.</p><button class="quiet" onclick={()=>resetPending=true}>Reset chrome controls</button>{#if resetPending}<div class="reset-prompt" role="group" aria-label="Confirm chrome reset"><p>Restore built-in visibility and ordering in this draft? Apply to save.</p><button class="quiet" onclick={()=>resetPending=false}>Keep chrome controls</button><button class="quiet" onclick={reset}>Reset chrome draft</button></div>{/if}</section>
</fieldset>
<footer class="apply-bar"><div><span class="status-dot" class:unsaved={dirty}></span>{dirty?'You have unapplied chrome changes':'Chrome preferences match the saved configuration'}</div><div class="apply-actions"><button class="quiet" disabled={busy||!dirty} onclick={discard}>Discard chrome changes</button><button class="primary" disabled={busy||!dirty} onclick={apply}>Apply chrome changes</button></div></footer>
{/if}
<style>
fieldset{padding:0;margin:0;border:0;min-width:0}.description{font-size:12px;color:var(--fd-text-muted);line-height:1.8;margin:12px 0 18px}.order-input{font:inherit;font-size:12px;border:1px solid var(--fd-border);border-radius:8px;padding:10px;background:var(--fd-surface);color:var(--fd-text);width:45%;max-width:340px}.setting-row>div{min-width:0}.configuration-path{overflow-wrap:anywhere;user-select:text}strong{font-size:13px}.reset-prompt{padding:20px;border:1px solid var(--fd-border);border-radius:var(--fd-radius);margin-top:18px}.reset-prompt p{font-size:12px;color:var(--fd-text-muted);line-height:1.8;margin-bottom:12px}.reset-prompt button{margin-right:8px}.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}@media(max-width:600px){.order-input{width:100%;max-width:none}}
</style>
