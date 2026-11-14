<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import type { NetworkSnapshot,NetworkAction,WifiNetwork } from './connectivity';
  let snapshot=$state<NetworkSnapshot|null>(null),busy=$state(false),error=$state(''),message=$state('');
  let selected=$state<WifiNetwork|null>(null),password=$state('');
  let confirmation=$state<{action:NetworkAction;message:string}|null>(null);
  let alive=true,polling=false,revision=0;
  $effect(()=>{if(confirmation)void tick().then(()=>document.getElementById('network-confirmation')?.focus());});
  $effect(()=>{if(selected)void tick().then(()=>{document.getElementById('network-connection')?.scrollIntoView({block:'center'});(document.getElementById('network-password')??document.getElementById('network-connect'))?.focus({preventScroll:true});});});
  async function refresh(clearError=false) {
    if (busy||polling) return; polling=true;const token=revision;
    try { const next=await invoke<NetworkSnapshot>('get_network');if(alive&&token===revision){snapshot=next;if(clearError)error='';} }
    catch(cause){if(alive&&token===revision)error=String(cause);}finally{polling=false;}
  }
  async function apply(action:NetworkAction) {
    if(busy)return;revision++;busy=true;error='';message='';confirmation=null;
    try { const next=await invoke<NetworkSnapshot>('apply_network',{action});if(alive){snapshot=next;message='Network state refreshed after the operation.';selected=null;} }
    catch(cause){if(alive)error=String(cause);}finally{password='';if(alive)busy=false;}
  }
  function connect(){if(selected){const secret=password;password='';void apply({type:'connect_wifi',id:selected.id,password:secret});}}
  onMount(()=>{void refresh();const timer=window.setInterval(()=>void refresh(),5000);return()=>{alive=false;password='';window.clearInterval(timer);};});
</script>
<div class="page-heading"><div><div class="eyebrow">STAY CONNECTED</div><h1>Network</h1><p>Manage wired connections and nearby Wi-Fi networks.</p></div><button class="quiet" disabled={busy} onclick={()=>refresh(true)}>Refresh</button></div>
{#if error}<div class="notice error" role="alert">{error}</div>{/if}
{#if message}<div class="notice success" role="status">{message}</div>{/if}
{#if confirmation}<section id="network-confirmation" tabindex="-1" class="connection-prompt" role="group" aria-label="Confirm network change"><h2>Change this connection?</h2><p>{confirmation.message}</p><button class="quiet" onclick={()=>confirmation=null}>Cancel</button><button class="primary" onclick={()=>{if(confirmation)void apply(confirmation.action);}}>Confirm network change</button></section>{/if}
{#if snapshot}
  <section class="section"><h2>Ethernet</h2><div class="settings-card">{#each snapshot.devices.filter(device=>device.kind==='ethernet') as device}<div class="setting-row"><div><strong>{device.name}</strong><p>{device.state} · {device.connection==='--'?'No active connection':device.connection}</p></div>{#if device.state.startsWith('connected')}<button class="quiet" disabled={busy} onclick={()=>confirmation={action:{type:'set_device',device:device.name,connected:false},message:`Disconnect ${device.name}? Downloads and remote sessions using this connection may stop.`}}>Disconnect</button>{:else}<button class="quiet" disabled={busy} onclick={()=>apply({type:'set_device',device:device.name,connected:true})}>Connect {device.name}</button>{/if}</div>{:else}<div class="setting-row"><p>No Ethernet devices found.</p></div>{/each}</div></section>
  <section class="section"><h2>Wi-Fi</h2><div class="settings-card"><div class="setting-row"><div><label for="network-wifi">Wi-Fi</label><p>NetworkManager manages wireless connections and their saved credentials.</p></div><input id="network-wifi" class="switch" type="checkbox" role="switch" checked={snapshot.wifi_enabled} disabled={busy} onchange={event=>{const enabled=event.currentTarget.checked;event.currentTarget.checked=!!snapshot?.wifi_enabled;if(enabled)void apply({type:'set_wifi',enabled:true});else confirmation={action:{type:'set_wifi',enabled:false},message:'Turn off Wi-Fi? Wireless connections and remote sessions may disconnect.'};}} /></div><div class="setting-row"><div><strong>Nearby networks</strong><p>Scanning happens only when you request it.</p></div><button class="quiet" disabled={busy||!snapshot.wifi_enabled} onclick={()=>apply({type:'rescan'})}>Scan for networks</button></div></div></section>
  <section class="section"><h2>Available Wi-Fi networks</h2><div class="settings-card">{#each snapshot.networks as network}<div class="setting-row"><div><strong>{network.ssid}</strong><p>{network.security==='--'||!network.security?'Open network':network.security} · {network.signal}% signal</p><p>{network.device} · {network.bssid}</p></div>{#if network.active}<button class="quiet" disabled={busy} onclick={()=>confirmation={action:{type:'set_device',device:network.device,connected:false},message:`Disconnect from ${network.ssid}? Remote sessions may stop.`}}>Connected · Disconnect</button>{:else}<button class="quiet" disabled={busy} aria-label={`Connect to ${network.ssid} on ${network.device}`} onclick={()=>{selected=network;password='';error='';}}>Connect</button>{/if}</div>{:else}<div class="setting-row"><p>{snapshot.wifi_enabled?'No nearby networks found. Try scanning.':'Turn on Wi-Fi to see nearby networks.'}</p></div>{/each}</div></section>
  {#if selected}<section id="network-connection" class="connection-prompt" role="group" aria-label="Wi-Fi connection"><h2>Connect to {selected.ssid}</h2><p>{selected.device} · {selected.bssid}</p>{#if selected.security&&selected.security!=='--'}<label for="network-password">Wi-Fi password</label><input id="network-password" type="password" autocomplete="off" maxlength="256" bind:value={password} /><p>Leave blank to try saved credentials. Enterprise networks may need an existing NetworkManager profile.</p>{/if}<div class="actions"><button class="quiet" disabled={busy} onclick={()=>{selected=null;password='';}}>Cancel connection</button><button id="network-connect" class="primary" disabled={busy} onclick={connect}>{busy?'Connecting…':'Connect to network'}</button></div></section>{/if}
  <p class="footnote">Changes take effect immediately through NetworkManager. Device permissions and system authorization still apply.</p>
{:else if !error}<div class="loading" role="status">Reading NetworkManager state…</div>{/if}
<style>
  strong{font-size:13px;font-weight:550}.connection-prompt{border:1px solid var(--fd-border);background:var(--fd-surface);border-radius:var(--fd-radius);padding:22px;margin:20px 0}.connection-prompt p{color:var(--fd-text-muted);font-size:12px;line-height:1.8;margin:12px 0}.connection-prompt label{display:block;font-size:12px;margin:15px 0 8px}.connection-prompt input{padding:10px;border:1px solid var(--fd-border);border-radius:8px;max-width:100%;background:var(--fd-surface);color:var(--fd-text)}.actions{display:flex;gap:10px;margin-top:18px}.primary{background:var(--fd-accent);color:white;padding:10px 14px;border-radius:8px;font-size:12px}.connection-prompt>.primary{margin-left:10px}.footnote{line-height:1.8}
</style>
