<script lang="ts">
  import { onMount } from 'svelte';
  import Displays from './Displays.svelte';
  import Sound from './Sound.svelte';
  import KeyboardInput from './KeyboardInput.svelte';
  import Preferences from './Preferences.svelte';
  import Accessibility from './Accessibility.svelte';
  import Network from './Network.svelte';
  import Bluetooth from './Bluetooth.svelte';
  import ThemeEditor from './ThemeEditor.svelte';
  import Calendar from './Calendar.svelte';
  import Printers from './Printers.svelte';
  import Applications from './Applications.svelte';
  import Chrome from './Chrome.svelte';
  import Workspaces from './Workspaces.svelte';
  import Debug from './Debug.svelte';
  import { sections, pageForPanel } from './navigation';
  import { invoke } from '@tauri-apps/api/core';
  import type { Page } from './navigation';
  import { getAppearance, applyAppearance, getAbout, openExistingSettings, sameAppearance } from './api';
  import type { Appearance, ThemeChoice, About } from './api';

  let page = $state<Page>('appearance');
  let saved = $state<Appearance | null>(null);
  let draft = $state<Appearance | null>(null);
  let themes = $state<ThemeChoice[]>([]);
  let about = $state<About | null>(null);
  let loading = $state(true);
  let busy = $state(false);
  let error = $state('');
  let message = $state('');
  let dirty = $derived(!!draft && !!saved && !sameAppearance(draft, saved));
  let selected = $derived(themes.find(theme => theme.id === draft?.theme));
  function navigate(next: Page) { page = next; error = ''; message = ''; }
  async function reload() {
    loading = true; error = ''; message = '';
    try {
      const state = await getAppearance();
      saved = { ...state.appearance }; draft = { ...state.appearance }; themes = state.themes;
    } catch (cause) { error = String(cause); }
    finally { loading = false; }
  }
  async function apply() {
    if (!saved || !draft || busy) return;
    busy = true; error = ''; message = '';
    try {
      const state = await applyAppearance({ ...saved }, { ...draft });
      saved = { ...state.appearance }; draft = { ...state.appearance }; themes = state.themes;
      message = 'Your appearance settings are applied.';
    } catch (cause) { error = String(cause); }
    finally { busy = false; }
  }
  async function openLegacy(panel: string) {
    try { await openExistingSettings(panel); }
    catch (cause) { error = String(cause); }
  }
  function reset() {
    if (saved) draft = { ...saved };
    error = ''; message = '';
  }
  onMount(() => {
    void reload();
    void invoke<string>('get_initial_panel').then(panel => page = pageForPanel(panel)).catch(cause => error = String(cause));
    void getAbout().then(value => about = value).catch(cause => error = String(cause));
  });
</script>

<svelte:window onfocus={() => { if (!dirty && !busy && !loading) void reload(); }} />

<div class="app-shell">
  <aside class="sidebar" aria-label="Settings navigation">
    <div class="brand"><span class="brand-mark" aria-hidden="true">f</span><div>FocalDesk<span>Make space for your work.</span></div></div>
    <div class="sidebar-label">YOUR DESKTOP</div>
    <nav aria-label="Settings sections">
      {#each sections as item}
        <button class:active={!!item.page && (page === item.page || (item.page === 'input' && page === 'pointer'))}
          aria-current={item.page && (page === item.page || (item.page === 'input' && page === 'pointer')) ? 'page' : undefined}
          aria-label={item.page ? item.name : `${item.name} — opens existing Settings`}
          title={item.page ? item.name : `${item.name} — opens existing Settings`}
          onclick={() => item.page ? navigate(item.page) : openLegacy(item.id)}>
          <span aria-hidden="true">{item.icon}</span>{item.name}
          {#if !item.page}<span class="external" aria-hidden="true">↗</span>{/if}
        </button>
      {/each}
    </nav>
    <div class="sidebar-foot"><span class="preview-dot"></span>Settings Preview<p>Your familiar settings, reimagined.</p></div>
  </aside>

  <main>
    <header class="topbar"><span>Settings <span class="breadcrumb">/ {sections.find(item => item.page === (page === 'pointer' ? 'input' : page))?.name}</span></span><span class="pill">PREVIEW</span></header>
    <div class="page-content">
      {#if error}<div class="notice error" role="alert"><span>{error}</span><button disabled={loading || busy} onclick={reload}>Reload</button></div>{/if}
      {#if message}<div class="notice success" role="status">✓ {message}</div>{/if}
      {#if page === 'appearance'}
        <div class="page-heading"><div><div class="eyebrow">A DESKTOP THAT FEELS LIKE YOU</div><h1>Appearance</h1><p>A little more personal. A little more you.</p></div><button class="quiet" onclick={() => navigate('editor')}>Theme editor</button></div>
        {#if loading && !draft}
          <div class="loading" role="status">Loading your desktop appearance…</div>
        {:else if draft}
          <section class="preview-card" aria-label="Appearance preview" style={`--preview-accent:${selected?.accent ?? '#635bdf'};--preview-surface:${selected?.surface ?? '#20263b'};--preview-text:${selected?.text ?? '#ffffff'};--preview-glow:${draft.glow_strength}`}>
            <div class="desktop-preview" class:focused={draft.output_focus_glow}>
              {#if selected?.wallpaper_preview}<img class="preview-wallpaper" src={selected.wallpaper_preview} alt="" />{/if}
              <div class="preview-rail" aria-hidden="true"><b>f</b><span>▦</span><span>▤</span><span>✉</span><span>◉</span><span class="rail-bottom">◷</span></div>
              <div class="orb orb-one"></div><div class="orb orb-two"></div>
              <div class="sample-window" style={`font-size:${14 * draft.font_scale}px`}><div class="sample-title"><span class="window-dots" aria-hidden="true">● ● ●</span><span>A space to make things</span></div><div class="sample-body"><span class="sample-icon" aria-hidden="true">✦</span><strong>Find your focus.</strong><p>Your ideas deserve a beautiful place to begin.</p><span class="sample-button">Make yourself at home</span></div></div>
              <span class="preview-clock" aria-hidden="true">09:41</span>
            </div>
            <div class="preview-caption"><span><span class="preview-dot"></span>{selected?.wallpaper_preview ? 'Wallpaper preview' : 'Local preview'} <span class="caption-muted">· {selected?.wallpaper_preview ? selected.wallpaper_hdr ? 'SDR tone-mapped · HDR source' : 'SDR wallpaper' : 'SDR illustration'}</span></span><span>{selected?.name ?? 'Current theme'}</span></div>
          </section>
          <section class="section" aria-labelledby="theme-title"><div class="section-heading"><div><h2 id="theme-title">Choose your atmosphere</h2><p>Select a theme to preview it. Apply when it feels right.</p></div><span class="count">{themes.length} themes</span></div>
            <div class="theme-grid">
              {#each themes as theme}
                <button class="theme-choice" class:selected={draft.theme === theme.id} aria-pressed={draft.theme === theme.id} disabled={busy} onclick={() => { if (draft) draft.theme = theme.id; message = ''; }}>
                  <div class="theme-swatch" style={`--swatch:${theme.accent};--swatch-surface:${theme.surface}`}><span class="swatch-window"></span><span class="swatch-rail"></span>{#if draft.theme === theme.id}<span class="check" aria-hidden="true">✓</span>{/if}</div>
                  <span class="theme-name">{theme.name}</span><span class="theme-source">{theme.source}{theme.dynamic_range === 'HDR' ? ' · HDR' : ''}</span>
                </button>
              {/each}
            </div>
          </section>
          <section class="section" aria-labelledby="chrome-title"><h2 id="chrome-title">Desktop surfaces</h2><div class="settings-card">
            <div class="setting-row"><div><label for="shader-chrome">Use shader chrome</label><p>Enable the desktop’s beveled and glass styling.</p></div><input id="shader-chrome" class="switch" type="checkbox" role="switch" disabled={busy} bind:checked={draft.shader_chrome} /></div>
            <div class="setting-row"><div><label for="work-area-glass">Work-area glass</label><p>Add a subtle tint and edge highlight behind windows.</p></div><input id="work-area-glass" class="switch" type="checkbox" role="switch" disabled={busy} bind:checked={draft.work_area_glass} /></div>
            <div class="setting-row"><div><label for="rail-backdrop">Rail background</label><p>Choose the backing behind the system rail.</p></div><select id="rail-backdrop" disabled={busy} bind:value={draft.rail_backdrop}><option value="solid">Solid</option><option value="smoked-glass">Smoked glass</option></select></div>
            <div class="setting-row"><div><label for="panel-darkness-default">Use theme panel darkness</label><p>Use the active theme’s rail and dock backing opacity.</p></div><input id="panel-darkness-default" class="switch" type="checkbox" role="switch" disabled={busy} checked={draft.panel_darkness===null} onchange={event=>{if(draft)draft.panel_darkness=event.currentTarget.checked?null:0.5;}} /></div>
            {#if draft.panel_darkness!==null}<div class="setting-row"><div><label for="panel-darkness">Panel darkness</label><p>Override rail and dock backing opacity; icons and text keep their opacity.</p></div><div class="range-control"><input id="panel-darkness" type="range" min="0" max="1" step="0.01" disabled={busy} bind:value={draft.panel_darkness} aria-valuetext={`${Math.round(draft.panel_darkness*100)} percent`} /><output for="panel-darkness">{Math.round(draft.panel_darkness*100)}%</output></div></div>{/if}
            <div class="setting-row"><div><label for="task-shelf-visibility">Task shelf visibility</label><p>Choose when the bottom application shelf moves out of the way.</p></div><select id="task-shelf-visibility" disabled={busy} bind:value={draft.task_shelf_visibility}><option value="always-visible">Always visible</option><option value="intelligent-dodge">Intelligent dodge</option><option value="autohide">Autohide</option></select></div>
          </div><p class="footnote">Surface and shelf changes apply to the desktop after Apply. The local illustration previews theme colors, text size, and glow.</p></section>
          <section class="section" aria-labelledby="details-title"><h2 id="details-title">The finer details</h2><div class="settings-card">
            <div class="setting-row"><div><label for="font-scale">Text size</label><p>A comfortable reading size across your desktop.</p></div><div class="range-control"><input id="font-scale" type="range" min="0.75" max="2" step="0.05" disabled={busy} bind:value={draft.font_scale} aria-valuetext={`${Math.round(draft.font_scale * 100)} percent`} /><output for="font-scale">{Math.round(draft.font_scale * 100)}%</output></div></div>
            <div class="setting-row"><div><label for="glow">Accent glow</label><p>Give your desktop chrome a softer edge.</p></div><div class="range-control"><input id="glow" type="range" min="0" max="1" step="0.05" disabled={busy} bind:value={draft.glow_strength} aria-valuetext={`${Math.round(draft.glow_strength * 100)} percent`} /><output for="glow">{Math.round(draft.glow_strength * 100)}%</output></div></div>
            <div class="setting-row"><div><label for="focus-glow">Highlight the focused display</label><p>A subtle accent to help you find your place.</p></div><input class="switch" id="focus-glow" type="checkbox" role="switch" disabled={busy} bind:checked={draft.output_focus_glow} /></div>
          </div></section>
          <p class="footnote">HDR wallpapers are tone-mapped here for an SDR preview; this does not show HDR brightness. Display calibration remains in Displays, and this preview does not change your active desktop.</p>
        {/if}
      {:else if page === 'displays'}
        <Displays />
      {:else if page === 'sound'}
        <Sound />
      {:else if page === 'input' || page === 'pointer'}
        {#key page}<KeyboardInput initialTab={page === 'pointer' ? 'pointer' : 'keyboard'} />{/key}
      {:else if page === 'privacy' || page === 'power'}
        {#key page}<Preferences section={page} />{/key}
      {:else if page === 'accessibility'}
        <Accessibility />
      {:else if page === 'network'}
        <Network />
      {:else if page === 'bluetooth'}
        <Bluetooth />
      {:else if page === 'editor'}
        <ThemeEditor />
      {:else if page === 'calendar'}
        <Calendar />
      {:else if page === 'printers'}
        <Printers />
      {:else if page === 'applications'}
        <Applications />
      {:else if page === 'chrome'}
        <Chrome />
      {:else if page === 'workspaces'}
        <Workspaces />
      {:else if page === 'debug'}
        <Debug />
      {:else}
        <div class="page-heading"><div><div class="eyebrow">BUILT FOR YOUR FLOW</div><h1>About FocalDesk</h1><p>Your desktop. Your space to create.</p></div></div>
        <section class="about-hero"><span class="brand-mark large" aria-hidden="true">f</span><h2>FocalDesk</h2><p>A thoughtful space for focused work.</p><span class="pill">ALPHA · SETTINGS PREVIEW {about?.version ?? ''}</span></section>
        <section class="settings-card about-details" aria-label="System information">
          {#if about}{#each [['Operating system', about.operating_system], ['Architecture', about.architecture], ['Session', about.session], ['Settings version', about.version]] as [label, value]}<div class="setting-row"><span>{label}</span><strong>{value}</strong></div>{/each}{:else}<div class="setting-row">Loading system information…</div>{/if}
        </section>
        <section class="settings-card" aria-label="Project information"><div class="setting-row"><div><strong>FocalDesk project</strong><p>Source code and license. Built with Smithay, Svelte, Tauri, GTK, WebKitGTK, PipeWire, and Rust.</p></div>{#each [['project','Project'],['license','License'],['credits','Credits']] as [resource,label]}<button class="quiet" onclick={() => invoke('open_project_resource', { resource }).catch(cause => error = String(cause))}>{label} ↗</button>{/each}</div></section>
        <section class="migration-card"><span aria-hidden="true">↗</span><div><h2>Advanced settings during preview</h2><p>Some advanced controls still open GTK Settings. It remains available while the preview is tested.</p></div><button class="quiet" onclick={() => openLegacy('appearance')}>Open existing Settings ↗</button></section>
      {/if}
    </div>
    {#if page === 'appearance' && draft}<footer class="apply-bar"><div><span class:unsaved={dirty} class="status-dot"></span>{dirty ? 'You have unapplied changes' : 'Your desktop is up to date'}</div><div class="apply-actions"><button class="quiet" disabled={!dirty || busy} onclick={reset}>Discard</button><button class="primary" disabled={!dirty || busy || loading} onclick={apply}>{busy ? 'Applying…' : 'Apply changes'}</button></div></footer>{/if}
  </main>
</div>

<style>
  .setting-row>select{font:inherit;font-size:12px;border:1px solid var(--fd-border);border-radius:8px;padding:10px 12px;background:var(--fd-surface);color:var(--fd-text);max-width:240px}
  .setting-row>select:focus-visible{outline:2px solid var(--fd-accent);outline-offset:4px}
</style>
