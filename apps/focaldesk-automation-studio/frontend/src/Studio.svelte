<script lang="ts">
  import { onMount } from 'svelte';
  import { SvelteFlow, Background, Controls, MiniMap, useSvelteFlow, addEdge, type Connection } from '@xyflow/svelte';
  import Block from './Block.svelte';
  import {catalog, defaults, starter, type Kind, type Block as BlockData, type Workflow} from './model';
  const nodeTypes = {block:Block};
  const {screenToFlowPosition,fitView} = useSvelteFlow();
  const initial = starter();
  let id = $state(initial.id), name = $state(initial.name);
  let nodes = $state(initial.nodes), edges = $state(initial.edges);
  let query = $state(''), busy = $state(false), notice = $state('Drag blocks onto the canvas, then connect their ports.');
  let isError = $state(false), dirty = $state(false), ready = $state(false);
  let savedFingerprint = $state('');
  let drafts = $state<{id:string;name:string}[]>([]), library = $state(false), preview = $state('');
  let selected = $derived(nodes.find(n=>n.selected));
  let meta = $derived(catalog.find(c=>c.kind===selected?.data.kind));
  let native = typeof window !== 'undefined' && '__TAURI__' in window;
  function workflow():Workflow {return {version:1,id,name,nodes:nodes.map(n=>({id:n.id,type:'block',position:n.position,data:n.data})),edges:edges.map(e=>({id:e.id,source:e.source,target:e.target,sourceHandle:e.sourceHandle,type:'smoothstep',label:e.sourceHandle==='yes'?'Yes':e.sourceHandle==='no'?'No':undefined}))};}
  let fingerprint = $derived(JSON.stringify(workflow()));
  $effect(()=>{if(ready) dirty = fingerprint!==savedFingerprint;});
  function report(text:string,error=false){notice=text;isError=error;}
  async function request(op:string,flow?:Workflow,loadId?:string):Promise<any> {
    if(native) {
      const w = window as unknown as {__TAURI__:{core:{invoke:(command:string,args:unknown)=>Promise<any>}}};
      return w.__TAURI__.core.invoke('studio_request',{op,flow:flow??null,id:loadId??null});
    }
    // Browser development persists drafts, while publishing requires the native app.
    const all:Record<string,Workflow> = JSON.parse(localStorage.getItem('focaldesk-studio-drafts')||'{}');
    if(op==='list')return Object.values(all).map(f=>({id:f.id,name:f.name}));
    if(op==='load') {if(!all[loadId!])throw new Error('Draft not found');return all[loadId!];}
    if(op==='save' && flow){all[flow.id]=flow;localStorage.setItem('focaldesk-studio-drafts',JSON.stringify(all));return;}
    throw new Error('Open the native FocalDesk app to validate or publish to the automation engine.');
  }
  async function operate(op:'save'|'validate'|'publish') {
    if(busy)return;
    busy=true; const snapshot=workflow(); const snapshotFingerprint=JSON.stringify(snapshot);
    try {
      const result=await request(op,snapshot);
      if(op==='validate'){preview=result.lua;report('Valid workflow. Review the generated Lua below.');}
      else {savedFingerprint=snapshotFingerprint;drafts=await request('list');report(op==='save'?'Draft saved.':'Published. Restart focaldesk-automation.service to load this revision.');}
    }catch(e){report(String(e),true);}finally{busy=false;}
  }
  function update(patch:Partial<BlockData>){if(!selected)return;nodes=nodes.map(n=>n.id===selected!.id?{...n,data:{...n.data,...patch}}:n);preview='';}
  function add(kind:Kind,position={x:200+nodes.length*12,y:100+nodes.length*65}) {
    nodes=[...nodes.map(n=>({...n,selected:false})),{id:crypto.randomUUID(),type:'block',position,data:{...defaults[kind]},selected:true}];preview='';
  }
  function connect(connection:Connection){edges=addEdge({...connection,type:'smoothstep',label:connection.sourceHandle==='yes'?'Yes':connection.sourceHandle==='no'?'No':undefined},edges);preview='';}
  function drop(event:DragEvent){event.preventDefault();const kind=event.dataTransfer?.getData('application/focaldesk-block') as Kind;if(catalog.some(c=>c.kind===kind))add(kind,screenToFlowPosition({x:event.clientX,y:event.clientY}));}
  function remove(){if(!selected)return;const target=selected.id;nodes=nodes.filter(n=>n.id!==target);edges=edges.filter(e=>e.source!==target&&e.target!==target);}
  async function load(loadId:string){if(dirty&&!confirm('Discard unsaved changes and open this draft?'))return;try{const flow:Workflow=await request('load',undefined,loadId);id=flow.id;name=flow.name;nodes=flow.nodes.map(n=>({...n,type:'block'}));edges=flow.edges;preview='';savedFingerprint=JSON.stringify(workflow());library=false;report('Draft opened.');setTimeout(()=>fitView({padding:0.3}),100);}catch(e){report(String(e),true);}}
  function newFlow(){if(dirty&&!confirm('Discard unsaved changes and create a workflow?'))return;const flow=starter();id=flow.id;name='Untitled workflow';nodes=flow.nodes;edges=flow.edges;preview='';library=false;savedFingerprint='';}
  onMount(()=>{savedFingerprint=JSON.stringify(workflow());ready=true;request('list').then(result=>drafts=result).catch(e=>report(String(e),true));const unload=(e:BeforeUnloadEvent)=>{if(dirty){e.preventDefault();e.returnValue='';}};window.addEventListener('beforeunload',unload);return()=>window.removeEventListener('beforeunload',unload);});
</script>

<svelte:window onkeydown={(e)=>{if((e.ctrlKey||e.metaKey)&&e.key==='s'){e.preventDefault();operate('save');}}}/>
<div class="app-shell">
  <header>
    <div class="brand-mark">⌘</div><div class="brand"><strong>FocalDesk</strong><span>Automation Studio</span></div>
    <span class="header-divider"></span><span class="workspace-label">PERSONAL WORKSPACE</span>
    <div class="header-actions"><span class="draft-state"><i class:unsaved={dirty}></i>{dirty?'Unsaved changes':'Draft'}</span><button disabled={busy} onclick={()=>operate('save')}>Save draft</button><button class="primary" disabled={busy} onclick={()=>operate('publish')}>{busy?'Working…':'Publish workflow'} <span>↗</span></button></div>
  </header>
  <div class="body">
    <nav class="rail" aria-label="Studio navigation"><div class="avatar">F</div><button class:active={!library} title="Workflow editor" onclick={()=>library=false}>▦</button><button class:active={library} title="Saved workflows" onclick={()=>library=true}>▤</button><span class="rail-spacer"></span><span class="rail-bottom" title="Local workspace">◉</span></nav>
    <aside class="palette">
      <div class="pane-title"><strong>{library?'Your workflows':'Block library'}</strong><span>{library?drafts.length:catalog.length}</span></div>
      {#if library}
        <button class="new-flow" onclick={newFlow}>＋ New workflow</button>
        {#each drafts as draft}<button class="draft-row" onclick={()=>load(draft.id)}><span>⌘</span><strong>{draft.name}</strong><small>Open draft ↗</small></button>{/each}
        {#if !drafts.length}<p class="empty">Saved workflows will appear here.</p>{/if}
      {:else}
        <label class="search"><span>⌕</span><input aria-label="Search blocks" placeholder="Search blocks…" bind:value={query}/><kbd>⌕</kbd></label>
        {#each ['Triggers','Logic & intelligence','Desktop actions'] as group}
          {@const items=catalog.filter(c=>c.group===group && `${c.label} ${c.description}`.toLowerCase().includes(query.toLowerCase()))}
          {#if items.length}<section class="block-group"><h2>{group}<span>⌄</span></h2><div class="palette-grid">{#each items as item}<button class="palette-item" draggable="true" ondragstart={(e)=>{e.dataTransfer?.setData('application/focaldesk-block',item.kind);if(e.dataTransfer)e.dataTransfer.effectAllowed='move';}} onclick={()=>add(item.kind)} title={item.description}><span class="palette-icon {item.color}">{item.icon}</span><span>{item.label}</span>{#if item.draft}<small>Draft only</small>{/if}</button>{/each}</div></section>{/if}
        {/each}
        <div class="palette-tip"><span>✧</span><strong>A little less repetition.</strong><p>Connect your desktop actions into a workflow that works for you.</p></div>
      {/if}
      <div class="palette-footer"><i></i> Local automation engine</div>
    </aside>
    <main>
      <div class="canvas-toolbar"><div><span class="breadcrumb">Workflows <span>/</span></span><input aria-label="Workflow name" bind:value={name} maxlength="160"/></div><button class="text-button" disabled={busy} onclick={()=>operate('validate')}>▷ Validate</button></div>
      <div class="canvas" role="region" aria-label="Workflow canvas" ondrop={drop} ondragover={(e)=>{e.preventDefault();if(e.dataTransfer)e.dataTransfer.dropEffect='move';}}>
        <SvelteFlow bind:nodes bind:edges {nodeTypes} onconnect={connect} fitView fitViewOptions={{padding:0.3}} minZoom={0.25} maxZoom={1.8} colorMode="dark" deleteKey={['Backspace','Delete']} defaultEdgeOptions={{type:'smoothstep',style:'stroke: #5c738c; stroke-width: 2;'}}>
          <Background gap={22} size={1} patternColor="#30343c"/>
          <Controls showLock={false}/>
          <MiniMap pannable zoomable nodeColor="#4278b0" maskColor="rgba(15,18,23,.8)"/>
        </SvelteFlow>
        <div class="canvas-caption">{nodes.length} blocks <span>·</span> {edges.length} connections <span>·</span> Drag to arrange · Scroll to zoom</div>
      </div>
      <div class="status-bar" class:error={isError} role="status"><span>{isError?'!':'◉'}</span>{notice}{#if !native}<em>Browser preview</em>{/if}</div>
    </main>
    <aside class="inspector">
      <div class="inspector-heading"><span>BLOCK SETTINGS</span><h1>{meta?.label??'Workflow settings'}</h1><small>{selected?'Configure this step of your workflow.':'Select a block to configure it.'}</small></div>
      <div class="inspector-content">
        {#if selected && meta}
          <div class="block-type"><span class="palette-icon {meta.color}">{meta.icon}</span><div><strong>{meta.label}</strong><small>{meta.group}</small></div><span class="type-tag">{meta.draft?'DRAFT':'BLOCK'}</span></div>
          {#if meta.draft}<p class="draft-warning">This block can be saved in a draft. Publishing is unavailable until the engine supports {selected.data.kind==='ai_decision'?'AI decisions':'webhook triggers'}.</p>{/if}
          <div class="form-fields">
          {#if selected.data.kind==='schedule'}
            <label>Schedule<input value={selected.data.schedule} oninput={(e)=>update({schedule:e.currentTarget.value})} placeholder="daily 09:00"/></label><p class="help">Use <code>daily HH:MM</code> or <code>every 15m</code>. Daily schedules use your local time zone.</p>
          {:else if selected.data.kind==='condition'}
            <label>Local hour is at least<input type="number" min="0" max="23" value={selected.data.hour} oninput={(e)=>update({hour:e.currentTarget.valueAsNumber})}/></label><p class="help">Connect the Yes and No ports to choose which actions run. Hours range from 0 to 23.</p>
          {:else if selected.data.kind==='notify'}
            <label>Notification title<input value={selected.data.title} oninput={(e)=>update({title:e.currentTarget.value})}/></label><label>Message<textarea rows="6" value={selected.data.body} oninput={(e)=>update({body:e.currentTarget.value})}></textarea></label><p class="help">Delivered through FocalDesk’s notification service.</p>
          {:else if selected.data.kind==='log'}
            <label>Log message<textarea rows="6" value={selected.data.message} oninput={(e)=>update({message:e.currentTarget.value})}></textarea></label><p class="help">View output in the automation service journal.</p>
          {:else if selected.data.kind==='volume'}
            <label>Volume <span>{selected.data.level}%</span><input type="range" min="0" max="100" value={selected.data.level} oninput={(e)=>update({level:e.currentTarget.valueAsNumber})}/></label>
          {:else if selected.data.kind==='wifi'||selected.data.kind==='bluetooth'}
            <label class="toggle-row">Enable {meta.label}<input type="checkbox" checked={selected.data.enabled} onchange={(e)=>update({enabled:e.currentTarget.checked})}/></label>
          {:else if selected.data.kind==='ai_decision'}
            <label>Decision prompt<textarea rows="7" value={selected.data.prompt} oninput={(e)=>update({prompt:e.currentTarget.value})}></textarea></label>
          {:else if selected.data.kind==='webhook'}
            <label>Event path<input value={selected.data.path} oninput={(e)=>update({path:e.currentTarget.value})}/></label>
          {/if}
          </div>
          <details class="payload"><summary>Block data</summary><pre>{JSON.stringify(selected.data,null,2)}</pre></details>
          <button class="delete-button" onclick={remove}>⌫ Remove block</button>
        {:else}<div class="empty-inspector">⌘<p>Every great workflow starts with a trigger.</p><small>Add blocks from the library and connect them from top to bottom.</small></div>{/if}
        {#if preview}<details class="payload" open><summary>Generated Lua</summary><pre>{preview}</pre></details>{/if}
      </div>
      <div class="inspector-footer"><span>◈</span><p>Runs locally on your desktop.<br/><small>Existing FocalDesk permissions apply.</small></p></div>
    </aside>
  </div>
</div>
